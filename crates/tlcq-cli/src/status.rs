//! `tlcq status` (design D6 Follows, D11; job-status spec): the agent's current
//! jobs, from its follows, each reported on the run it placed or attached to.
//!
//! The snapshot (records, follows, job locks) is read under the placement lock,
//! the VM is called without it, and the refresh is applied under it again, each
//! record only if its generation is unchanged since the snapshot.

use std::collections::{BTreeMap, BTreeSet};

use crate::entry::{Key, Label};
use crate::eta::Source;
use crate::files::read_json;
use crate::laptop_runs::Progress;
use crate::outcome::{JobResult, Outcome};
use crate::proto::{PreflightReply, PreflightRequest, RunAsk, VmJob, VmState};
use crate::refresh::refresh;
use crate::remote::{CallError, Remote};
use crate::snapshot::{self, Error};
use crate::store::{Follow, Followed, JobRecord, RunRef, State, Store};

/// What a reported run is.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "one per reported line, a few per command"
)]
pub enum View {
    /// On the VM: queued, running or stopping, as the VM said just now.
    Vm(VmJob),
    /// On the VM as last known: the VM is unreachable.
    LastKnown(State),
    /// On the laptop: waiting for the slot or the disk, or running.
    Laptop(Progress),
    /// Ended.
    Ended(JobResult),
    /// Sent with a commit whose outcome is still unknown.
    Sent,
    /// A commit that never reached the VM.
    NeverSent,
    /// Neither the laptop nor the VM knows it.
    Unknown,
}

/// One reported run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The configuration, if a follow names it.
    pub key: Option<Key>,
    /// The job.
    pub id: String,
    /// The run, when known.
    pub run: Option<String>,
    /// What it is.
    pub view: View,
    /// While it runs, its estimated end.
    pub eta: Option<LineEta>,
    /// Its engine, when the laptop holds its description.
    pub engine: Option<crate::engine::Engine>,
}

/// A running job's estimated end (D15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineEta {
    /// The seconds left after TLC's last progress line.
    pub remaining_secs: u64,
    /// Where the estimate comes from.
    pub source: Source,
}

/// What `status` reports, and its exit code.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// The runs, in follow order.
    pub lines: Vec<Line>,
    /// Whether the VM was unreachable, or its poll kept failing.
    pub unreachable: bool,
    /// Why the poll kept failing, when it did.
    pub vm_error: Option<String>,
    /// The VM's state volume, as its reply gave it.
    pub vm_disk: Option<crate::proto::Disk>,
    /// 3 when a run failed (`timeout`, `cancelled`, `error`, `never sent`), else
    /// 1 when one is `unexpected`, else 0.
    pub exit: u8,
}

/// How many times a poll that connects and fails is tried.
const POLL_TRIES: usize = 3;

/// Report the agent `agent`'s current jobs, or the jobs `ids`.
///
/// # Errors
/// If the laptop's state cannot be read or written.
pub fn status(
    store: &Store,
    remote: &mut impl Remote,
    versions: &crate::engine::Versions,
    agent: &Label,
    ids: &[String],
) -> Result<Status, Error> {
    // A record that changed while the VM was asked (a command placed or
    // cancelled meanwhile) is not refreshed from that reply: poll again (D6).
    let mut polls = 0;
    let (vm, vm_error, lock, current, generations) = loop {
        let (req, generations) = poll_request(store, versions, agent, ids)?;
        let (vm, vm_error) = poll(remote, &req);
        let lock = store.placement_lock()?;
        let current = store.records()?;
        polls += 1;
        let changed = current
            .iter()
            .any(|r| generations.get(&r.id) != Some(&r.generation));
        if !(changed && vm.is_some() && polls < POLL_TRIES) {
            break (vm, vm_error, lock, current, generations);
        }
    };
    let mut records = BTreeMap::new();
    for mut r in current {
        let reply = vm
            .as_ref()
            .filter(|_| generations.get(&r.id) == Some(&r.generation));
        // A laptop job needs no reply: its lock, read now, is its state (D7).
        if reply.is_some() || r.state == State::Laptop {
            let change = refresh(&r, store.job_lock_held(&r.id)?, reply);
            snapshot::apply(store, &lock, &mut r, change, reply)?;
        }
        if let Some(vm) = reply {
            snapshot::ended_runs(store, &lock, &mut r, vm)?;
        }
        records.insert(r.id.clone(), r);
    }
    if let Some(vm) = &vm {
        snapshot::resolve_follows(store, &lock, store.follows()?, vm)?;
    }
    let follows: Vec<Follow> = store
        .follows()?
        .into_iter()
        .filter(|f| f.label.same_agent(agent))
        .collect();
    drop(lock);
    let view = |f: &Follow| follow_view(f, records.get(&f.id), vm.as_ref());
    let mut lines = Vec::new();
    if ids.is_empty() {
        let latest: BTreeMap<&Key, u64> = follows.iter().fold(BTreeMap::new(), |mut m, f| {
            let n = m.entry(&f.key).or_insert(f.number);
            *n = (*n).max(f.number);
            m
        });
        for f in &follows {
            let line = line_of(f, view(f));
            let record = records.get(&f.id);
            if latest.get(&f.key) == Some(&f.number) || still_holds(&line, &f.key, record) {
                lines.push(line);
            }
        }
    } else {
        for id in ids {
            let mine = follows
                .iter()
                .filter(|f| &f.id == id)
                .max_by_key(|f| f.number);
            lines.push(match mine {
                Some(f) => line_of(f, view(f)),
                None => latest_run(id, records.get(id), vm.as_ref()),
            });
        }
    }
    from_run_dirs(store, &mut lines)?;
    crate::estimates::with_etas(store, &records, &mut lines)?;
    with_engines(&records, &mut lines);
    let exit = lines.iter().map(|l| exit_of(&l.view)).max().unwrap_or(0);
    Ok(Status {
        lines,
        unreachable: vm.is_none(),
        vm_disk: vm.as_ref().and_then(|v| v.disk),
        vm_error,
        exit,
    })
}

/// Give each line the engine of its job, from the laptop's record of it.
pub fn with_engines(records: &BTreeMap<String, JobRecord>, lines: &mut [Line]) {
    for line in lines {
        line.engine = records
            .get(&line.id)
            .and_then(|r| r.info.as_ref())
            .map(|i| i.engine);
    }
}

/// The VM's reply to `req` (`None`: unreachable), and why the poll failed if
/// it kept failing. A poll that fails is retried (D6); one that keeps failing
/// reports the last known states, as for an unreachable VM.
fn poll(
    remote: &mut impl Remote,
    req: &PreflightRequest,
) -> (Option<PreflightReply>, Option<String>) {
    let mut vm_error = None;
    for _ in 0..POLL_TRIES {
        match remote.status(req) {
            Ok(reply) => return (Some(reply), None),
            Err(CallError::Unreachable(_)) => break,
            Err(CallError::Failed(e)) => vm_error = Some(e),
        }
    }
    (None, vm_error)
}

/// The poll's request, read under the placement lock (released on return),
/// and each record's generation then.
fn poll_request(
    store: &Store,
    versions: &crate::engine::Versions,
    agent: &Label,
    ids: &[String],
) -> Result<(PreflightRequest, BTreeMap<String, u64>), Error> {
    let _lock = store.placement_lock()?;
    let records = store.records()?;
    let mut locked = BTreeSet::new();
    for r in &records {
        if store.job_lock_held(&r.id)? {
            locked.insert(r.id.clone());
        }
    }
    let follows = store.follows()?;
    let mut req = snapshot::request(versions, ids, Vec::new(), &records, &locked, &follows);
    let mine = follows.iter().filter(|f| f.label.same_agent(agent));
    req.ids.extend(mine.clone().map(|f| f.id.clone()));
    req.ids.sort();
    req.ids.dedup();
    // The records' earlier runs still to end, and each run a follow names.
    let asked: BTreeSet<RunAsk> = req
        .runs
        .drain(..)
        .chain(mine.filter_map(|f| match &f.what {
            Followed::Run(RunRef::Run(run)) => Some(RunAsk {
                id: f.id.clone(),
                run: run.clone(),
            }),
            _ => None,
        }))
        .collect();
    req.runs = asked.into_iter().collect();
    let generations: BTreeMap<String, u64> = records
        .iter()
        .map(|r| (r.id.clone(), r.generation))
        .collect();
    Ok((req, generations))
}

/// A follow's line.
fn line_of(f: &Follow, view: View) -> Line {
    let run = match &f.what {
        Followed::Run(RunRef::Run(run)) => Some(run.clone()),
        _ => None,
    };
    Line {
        key: Some(f.key.clone()),
        id: f.id.clone(),
        run,
        view,
        eta: None,
        engine: None,
    }
}

/// Whether an older follow's run is still queued or running with its key's
/// entry (or still unresolved): then it is reported beside the latest. A
/// laptop run is read from its record's entries; a VM run the VM could not be
/// asked about, from the record's mirror and pending entries.
fn still_holds(line: &Line, key: &Key, record: Option<&JobRecord>) -> bool {
    let holds = |entries: &[crate::entry::Entry]| entries.iter().any(|e| &e.key == key);
    match &line.view {
        View::Vm(job) => {
            matches!(job.state, VmState::Queued | VmState::Running) && holds(&job.entries)
        }
        View::Laptop(_) => record.is_some_and(|r| holds(&r.entries)),
        View::LastKnown(_) => record
            .is_some_and(|r| holds(&r.mirror) || r.pending.iter().any(|p| &p.entry.key == key)),
        View::Sent => true,
        View::Ended(_) | View::NeverSent | View::Unknown => false,
    }
}

/// What a follow follows, now.
fn follow_view(f: &Follow, record: Option<&JobRecord>, vm: Option<&PreflightReply>) -> View {
    let run = match &f.what {
        Followed::Result(r) => return View::Ended(r.clone()),
        Followed::NeverSent => return View::NeverSent,
        Followed::Run(RunRef::Token(_)) => return View::Sent,
        Followed::Run(RunRef::Run(run)) => run,
    };
    let record = record.filter(|r| r.run.as_ref() == Some(run));
    if let Some(vm) = vm {
        // A run asked about by name; or, for a follow whose token this poll
        // resolved, the id's latest run, if it is that run.
        let asked = vm
            .runs
            .iter()
            .find(|(a, _)| a.id == f.id && &a.run == run)
            .map(|(_, j)| j);
        let latest = vm.jobs.get(&f.id).filter(|j| j.run.as_ref() == Some(run));
        if let Some(job) = asked.or(latest).filter(|j| j.state != VmState::Unknown) {
            return vm_view(job);
        }
    }
    // A run the reply did not cover (placed while it was asked): as last known.
    match record {
        Some(r) => record_view(r, true),
        None => View::Unknown,
    }
}

/// An id's latest run, for an id the agent does not follow.
fn latest_run(id: &str, record: Option<&JobRecord>, vm: Option<&PreflightReply>) -> Line {
    let job = vm
        .and_then(|v| v.jobs.get(id))
        .filter(|j| j.state != VmState::Unknown);
    let (run, view) = match (job, record) {
        (Some(job), _) => (job.run.clone(), vm_view(job)),
        (None, Some(r)) => (r.run.clone(), record_view(r, vm.is_none())),
        (None, None) => (None, View::Unknown),
    };
    Line {
        key: None,
        id: id.to_owned(),
        run,
        view,
        eta: None,
        engine: None,
    }
}

/// A run as the VM reported it.
pub(crate) fn vm_view(job: &VmJob) -> View {
    match &job.state {
        VmState::Cached { result } | VmState::Ended { result } => View::Ended(result.clone()),
        VmState::Unknown => View::Unknown,
        _ => View::Vm(job.clone()),
    }
}

/// A run as the laptop's record of it says.
pub(crate) fn record_view(r: &JobRecord, unreachable: bool) -> View {
    match (r.state, &r.result) {
        (State::Ended, Some(result)) => View::Ended(result.clone()),
        (State::Laptop, _) => View::Laptop(Progress::default()),
        (State::Sent, _) => View::Sent,
        (state, _) if unreachable => View::LastKnown(state),
        _ => View::Unknown,
    }
}

/// What the laptop's run directories add: each laptop run's progress, and the
/// result of a laptop run its job's record no longer follows (another run of
/// the id placed since).
///
/// # Errors
/// If a file cannot be read.
pub(crate) fn from_run_dirs(store: &Store, lines: &mut [Line]) -> std::io::Result<()> {
    for l in lines {
        let Some(run) = l.run.as_deref() else {
            continue;
        };
        match &mut l.view {
            View::Laptop(p) => *p = Progress::read(store, &l.id, run)?,
            View::Unknown => {
                if let Some(r) = read_json(&store.run_dir(&l.id, run).join("result.json"))? {
                    l.view = View::Ended(r);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A run's part in the exit code.
fn exit_of(view: &View) -> u8 {
    match view {
        View::Ended(r) => r.outcome.exit_code(),
        View::Vm(job) if job.state == VmState::Stopping => 3,
        View::NeverSent | View::Unknown => Outcome::NeverSent.exit_code(),
        _ => 0,
    }
}

#[cfg(test)]
#[path = "status_tests.rs"]
pub(crate) mod tests;

//! The preflight and the refresh it drives (design D6, Preflight), under the
//! placement lock: one VM call, then every record and follow brought up to date,
//! and the laptop's side of the plan read off the result.

use std::collections::BTreeSet;
use std::time::Duration;

use crate::entry::Key;
use crate::history::{self, History};
use crate::laptop_runs::{orphan, Phase, Progress};
use crate::outcome::{JobResult, Outcome};
use crate::plan::Laptop;
use crate::proto::{PreflightReply, PreflightRequest, RunAsk, VmJob, VmState};
use crate::refresh::{refresh, resolve_token, Change};
use crate::remote::{CallError, Remote};
use crate::store::{Follow, Followed, JobRecord, PlacementLock, RunRef, State, Store};

/// Why the snapshot could not be taken.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The laptop's state could not be read or written.
    #[error("the laptop's state: {0}")]
    Io(#[from] std::io::Error),
    /// The history could not be written.
    #[error(transparent)]
    History(#[from] history::Error),
    /// The preflight connected, then failed: exit 3 with nothing written.
    #[error("the preflight: {0}")]
    Preflight(CallError),
}

/// What the plan reads: the laptop after the refresh, and the VM's reply
/// (`None`: unreachable).
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The laptop's side.
    pub laptop: Laptop,
    /// The VM's reply.
    pub vm: Option<PreflightReply>,
}

/// Take the snapshot for a command placing `ids` under `keys`, with `slots`
/// laptop slots.
///
/// # Errors
/// If the state cannot be read or written, or the preflight connects and fails.
pub fn take(
    store: &Store,
    lock: &PlacementLock,
    remote: &mut impl Remote,
    versions: &crate::engine::Versions,
    ids: &[String],
    keys: Vec<Key>,
    slots: usize,
) -> Result<Snapshot, Error> {
    let records = store.records()?;
    let mut locked = BTreeSet::new();
    for r in &records {
        if store.job_lock_held(&r.id)? {
            locked.insert(r.id.clone());
        }
    }
    let follows = store.follows()?;
    let req = request(versions, ids, keys, &records, &locked, &follows);
    let vm = match remote.preflight(&req) {
        Ok(reply) => Some(reply),
        Err(CallError::Unreachable(_)) => None,
        Err(e) => return Err(Error::Preflight(e)),
    };
    let mut laptop = Laptop::default();
    for mut r in records {
        let change = refresh(&r, locked.contains(&r.id), vm.as_ref());
        apply(store, lock, &mut r, change, vm.as_ref())?;
        if let Some(vm) = &vm {
            ended_runs(store, lock, &mut r, vm)?;
        }
        laptop.records.insert(r.id.clone(), r);
    }
    if let Some(vm) = &vm {
        resolve_follows(store, lock, follows, vm)?;
    }
    for id in ids {
        if let Some(result) = store.cached(id)? {
            laptop.cache.insert(id.clone(), result);
        }
    }
    // Every laptop job with a live supervisor holds a slot, waiting or running;
    // one running TLC (or moving it to the VM) counts as running.
    let live: Vec<&JobRecord> = laptop
        .records
        .values()
        .filter(|r| locked.contains(&r.id))
        .collect();
    for r in &live {
        let running = match r.state {
            State::Laptop => {
                let run = r.run.as_deref().unwrap_or_default();
                Progress::read(store, &r.id, run)?.phase == Phase::Running
            }
            State::Moving => true,
            _ => false,
        };
        if running {
            laptop.running.insert(r.id.clone());
        }
    }
    let waiting_or_running = live.iter().filter(|r| r.state == State::Laptop).count();
    laptop.free_slots = slots.saturating_sub(waiting_or_running);
    laptop.locked = locked;
    Ok(Snapshot { laptop, vm })
}

/// The preflight's request: the batch's ids and keys, every id the laptop holds
/// as `vm`, `sent` or lock-free `moving`, and every token a record or follow holds.
pub(crate) fn request(
    versions: &crate::engine::Versions,
    ids: &[String],
    keys: Vec<Key>,
    records: &[JobRecord],
    locked: &BTreeSet<String>,
    follows: &[Follow],
) -> PreflightRequest {
    let mut all: BTreeSet<String> = ids.iter().cloned().collect();
    let mut tokens = BTreeSet::new();
    for r in records {
        let held = match r.state {
            State::Vm | State::Sent => true,
            State::Moving => !locked.contains(&r.id),
            State::Laptop | State::Ended => false,
        };
        if held {
            all.insert(r.id.clone());
            tokens.extend(r.token.clone());
        }
    }
    for f in follows {
        if let Followed::Run(RunRef::Token(t)) = &f.what {
            tokens.insert(t.clone());
        }
    }
    let runs = records
        .iter()
        .flat_map(|r| {
            r.ending.iter().map(|run| RunAsk {
                id: r.id.clone(),
                run: run.clone(),
            })
        })
        .collect();
    PreflightRequest {
        versions: versions.clone(),
        ids: all.into_iter().collect(),
        keys,
        tokens: tokens.into_iter().collect(),
        runs,
    }
}

/// Apply one record's refresh; an orphan ends by the orphan rule (D7).
///
/// # Errors
/// If the record, the cache or the history cannot be written.
pub fn apply(
    store: &Store,
    lock: &PlacementLock,
    record: &mut JobRecord,
    change: Change,
    vm: Option<&PreflightReply>,
) -> Result<(), Error> {
    let job = vm.and_then(|v| v.jobs.get(&record.id));
    match change {
        Change::Keep => Ok(()),
        Change::Orphan => orphan(store, lock, record),
        Change::Vm => {
            record.state = State::Vm;
            record.token = None;
            if let Some(job) = job {
                record.take_run(job.run.clone());
                record.mirror.clone_from(&job.entries);
            }
            Ok(store.write_record(lock, record)?)
        }
        Change::End(result) => {
            // The VM's end of the id's latest run (not a failed move's laptop
            // timeout, nor a send that never arrived): the record follows it.
            let vm_end = result.outcome != Outcome::NeverSent && result.place != "laptop";
            if let Some(job) = job.filter(|_| vm_end) {
                if let Some(run) = &job.run {
                    keep_curve(store, &record.id, run, job)?;
                }
                record.take_run(job.run.clone());
            }
            end(store, lock, record, &result)
        }
    }
}

/// Record in the history the end of each earlier run of `record` the VM now
/// reports ended, and stop asking about it; one the VM does not know is
/// dropped, one the reply did not answer kept for the next.
///
/// # Errors
/// If the record or the history cannot be written.
pub fn ended_runs(
    store: &Store,
    lock: &PlacementLock,
    record: &mut JobRecord,
    vm: &PreflightReply,
) -> Result<(), Error> {
    if record.ending.is_empty() {
        return Ok(());
    }
    let mut still = Vec::new();
    for run in std::mem::take(&mut record.ending) {
        let job = vm
            .runs
            .iter()
            .find(|(a, _)| a.id == record.id && a.run == run)
            .map(|(_, j)| &j.state);
        match job {
            Some(VmState::Ended { result } | VmState::Cached { result }) => {
                if let Some((_, job)) = vm
                    .runs
                    .iter()
                    .find(|(a, _)| a.id == record.id && a.run == run)
                {
                    keep_curve(store, &record.id, &run, job)?;
                }
                let earlier = JobRecord {
                    run: Some(run),
                    ..record.clone()
                };
                if let Some(entry) = history_record(store, &earlier, result)? {
                    History::append(&store.history(), &entry)?;
                }
            }
            // Not answered (asked of no reply yet: it just left the record) or
            // not ended: asked about again.
            Some(VmState::Queued | VmState::Running | VmState::Stopping) | None => still.push(run),
            Some(VmState::Unknown) => {}
        }
    }
    record.ending = still;
    store.write_record(lock, record)?;
    Ok(())
}

/// End `record` with `result`: the record, then the cache and the history.
///
/// # Errors
/// If the record, the cache or the history cannot be written.
pub fn end(
    store: &Store,
    lock: &PlacementLock,
    record: &mut JobRecord,
    result: &JobResult,
) -> Result<(), Error> {
    record.state = State::Ended;
    record.token = None;
    record.pending.clear();
    record.result = Some(result.clone());
    store.write_record(lock, record)?;
    if result.outcome.cached() {
        store.cache(&record.id, result)?;
    }
    if let Some(entry) = history_record(store, record, result)? {
        History::append(&store.history(), &entry)?;
    }
    Ok(())
}

/// Keep the progress curve the VM sent with run `run` of `id`.
fn keep_curve(store: &Store, id: &str, run: &str, job: &VmJob) -> std::io::Result<()> {
    match &job.curve {
        Some(c) => std::fs::write(store.curve_path(id, run, c.attempt), &c.text),
        None => Ok(()),
    }
}

/// The history's record of a run's end, its curves those the laptop keeps;
/// none for a run that never ran.
fn history_record(
    store: &Store,
    record: &JobRecord,
    result: &JobResult,
) -> std::io::Result<Option<history::Record>> {
    let (Some(info), Some(end), Some(outcome)) = (
        record.info.as_ref(),
        record.run.clone(),
        outcome_name(&result.outcome),
    ) else {
        return Ok(None);
    };
    let curves = store.curves_of(&record.id, &end)?;
    let finished = result.outcome.cached();
    let duration = Duration::from_secs(result.duration_secs);
    Ok(Some(history::Record {
        id: record.id.clone(),
        end,
        names: info.names.clone(),
        expected: history::implied(duration, finished, result.judged_max()),
        lower_bound: !finished,
        duration_secs: result.duration_secs,
        outcome: outcome.to_owned(),
        place: result.place.clone(),
        curves,
        lanes: result.lanes.clone(),
    }))
}

/// An outcome's name in the history; none for `never sent`, which never ran.
fn outcome_name(outcome: &Outcome) -> Option<&'static str> {
    Some(match outcome {
        Outcome::Ok => "ok",
        Outcome::Unexpected => "unexpected",
        Outcome::Timeout => "timeout",
        Outcome::Cancelled { .. } => "cancelled",
        Outcome::Error { .. } => "error",
        Outcome::NeverSent => return None,
    })
}

/// Rewrite each follow still holding a token the VM answered for to what that
/// token reached.
///
/// # Errors
/// If a follow cannot be written.
pub fn resolve_follows(
    store: &Store,
    lock: &PlacementLock,
    follows: Vec<Follow>,
    vm: &PreflightReply,
) -> std::io::Result<()> {
    for mut f in follows {
        let Followed::Run(RunRef::Token(t)) = &f.what else {
            continue;
        };
        if let Some(what) = resolve_token(t, &f.id, vm) {
            f.what = what;
            store.write_follow(lock, &f)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;

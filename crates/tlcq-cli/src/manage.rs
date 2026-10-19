//! `tlcq ls`, `tlcq cancel`, `tlcq resume` and `tlcq fetch` (design D1, D7,
//! D10; job-status spec). `cancel`, `resume` and `fetch` act on the run the
//! agent follows: its follow of the id with the highest placement number, else
//! the id's latest run. A follow of a cached result or a commit that never ran
//! touches nothing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::entry::{Key, Label};
use crate::files::read_json;
use crate::laptop_runs::orphan;
use crate::outcome::JobResult;
use crate::proto::{
    CancelRequest, FetchReply, FetchRequest, PreflightReply, ResumeRequest, VmState,
};
use crate::refresh::refresh;
use crate::remote::{CallError, Remote};
use crate::runner::store::CancelMarker;
use crate::snapshot::{self, Error};
use crate::status::{from_run_dirs, record_view, vm_view, Line, Status, View};
use crate::store::{Followed, JobRecord, RunRef, State, Store};
use crate::submit::Supervisors;

/// Every job: the VM's (its latest run each) and the laptop's records. While
/// the VM is unreachable, its jobs show their last known state.
///
/// # Errors
/// If the laptop's state cannot be read, or the VM call connects and fails.
pub fn ls(store: &Store, remote: &mut impl Remote) -> Result<Status, Error> {
    let vm = match remote.ls() {
        Ok(jobs) => Some(jobs),
        Err(CallError::Unreachable(_)) => None,
        Err(e) => return Err(Error::Preflight(e)),
    };
    let records = {
        let lock = store.placement_lock()?;
        let mut records = store.records()?;
        for r in &mut records {
            if r.state == State::Laptop && !store.job_lock_held(&r.id)? {
                orphan(store, &lock, r)?;
            }
        }
        records
    };
    let mut ids: BTreeSet<&String> = records.iter().map(|r| &r.id).collect();
    ids.extend(vm.iter().flat_map(|jobs| jobs.keys()));
    let mut lines: Vec<Line> = ids
        .into_iter()
        .map(|id| {
            let job = vm
                .as_ref()
                .and_then(|jobs| jobs.get(id))
                .filter(|j| j.state != VmState::Unknown);
            let record = records.iter().find(|r| &r.id == id);
            let (key, run, view) = match (job, record) {
                (Some(job), _) => (first_key(&job.entries), job.run.clone(), vm_view(job)),
                (None, Some(r)) => (
                    first_key(if r.entries.is_empty() {
                        &r.mirror
                    } else {
                        &r.entries
                    }),
                    r.run.clone(),
                    record_view(r, vm.is_none()),
                ),
                (None, None) => (None, None, View::Unknown),
            };
            Line {
                key,
                id: id.clone(),
                run,
                view,
                eta: None,
                engine: None,
            }
        })
        .collect();
    from_run_dirs(store, &mut lines)?;
    let by_id = records.into_iter().map(|r| (r.id.clone(), r)).collect();
    crate::status::with_engines(&by_id, &mut lines);
    Ok(Status {
        lines,
        unreachable: vm.is_none(),
        vm_error: None,
        vm_disk: None,
        exit: 0,
    })
}

/// The full id a report's short id (any unique prefix) names: matched against
/// the laptop's records and follows; a prefix matching none is passed on as
/// given, for the VM to know or not.
///
/// # Errors
/// A prefix that matches several ids, or the state cannot be read.
pub fn resolve(store: &Store, prefix: &str) -> Result<Result<String, String>, Error> {
    let mut ids: BTreeSet<String> = store.records()?.into_iter().map(|r| r.id).collect();
    ids.extend(store.follows()?.into_iter().map(|f| f.id));
    let matching: Vec<&String> = ids.iter().filter(|id| id.starts_with(prefix)).collect();
    Ok(match matching.as_slice() {
        [] => Ok(prefix.to_owned()),
        [one] => Ok((*one).clone()),
        many => Err(format!(
            "{prefix}: names {} jobs; give more of its id",
            many.len()
        )),
    })
}

/// The first entry's key, to name a job in a listing.
fn first_key(entries: &[crate::entry::Entry]) -> Option<Key> {
    entries.first().map(|e| e.key.clone())
}

/// What a `cancel` or `fetch` came to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(clippy::large_enum_variant, reason = "one per command, returned once")]
pub enum Done {
    /// Done: the run's state after, and the files written (for `fetch`).
    Done {
        /// The run's state.
        line: Line,
        /// The files written.
        files: Vec<PathBuf>,
    },
    /// Refused, with nothing done: exit 2.
    Refused(String),
}

impl Done {
    /// The exit code: 0 when done, 2 when refused.
    pub fn exit(&self) -> u8 {
        match self {
            Done::Done { .. } => 0,
            Done::Refused(_) => 2,
        }
    }
}

/// The run to act on (`None`: the id's latest), or the answer outright.
type Target = Result<Option<String>, Done>;

/// The run of `id` the agent `agent` follows (`None` when it follows none:
/// then the id's latest), and the id's record. A follow that names no run is
/// answered outright: its cached result, a commit that never ran, or a
/// refusal while its commit's outcome is unknown.
fn target(store: &Store, agent: &Label, id: &str) -> Result<(Target, Option<JobRecord>), Error> {
    let follow = store
        .follows()?
        .into_iter()
        .filter(|f| f.id == id && f.label.same_agent(agent))
        .max_by_key(|f| f.number);
    let answer = |view| Done::Done {
        line: line(id, None, view),
        files: Vec::new(),
    };
    let run = match follow.map(|f| f.what) {
        None => Ok(None),
        Some(Followed::Run(RunRef::Run(run))) => Ok(Some(run)),
        Some(Followed::Run(RunRef::Token(_))) => Err(Done::Refused(
            "its place is uncertain: its commit's outcome is not known yet".to_owned(),
        )),
        Some(Followed::Result(r)) => Err(answer(View::Ended(r))),
        Some(Followed::NeverSent) => Err(answer(View::NeverSent)),
    };
    Ok((run, store.record(id)?))
}

/// Why a job cannot be acted on from its laptop record, if it cannot.
fn uncertain(record: Option<&JobRecord>, locked: bool) -> Option<String> {
    match record.map(|r| r.state) {
        Some(State::Sent) => {
            Some("its place is uncertain: its commit's outcome is not known yet".to_owned())
        }
        Some(State::Moving) if !locked => {
            Some("its place is uncertain: it is moving to the VM".to_owned())
        }
        _ => None,
    }
}

/// Cancel the run of `id` the agent follows, as `agent` (D7, D10): a laptop
/// job through `sups`, before this returns; else through the VM.
///
/// # Errors
/// If the laptop's state cannot be read or written, or the VM call connects
/// and fails.
pub fn cancel(
    store: &Store,
    remote: &mut impl Remote,
    sups: &mut impl Supervisors,
    agent: &Label,
    id: &str,
) -> Result<Done, Error> {
    let (run, generation) = {
        let lock = store.placement_lock()?;
        let (run, record) = target(store, agent, id)?;
        let run = match run {
            Ok(run) => run,
            Err(done) => return Ok(done),
        };
        if let Some(why) = uncertain(record.as_ref(), store.job_lock_held(id)?) {
            return Ok(Done::Refused(why));
        }
        let own = |r: &&JobRecord| run.is_none() || r.run == run;
        if let Some(r) = record
            .as_ref()
            .filter(|r| r.state == State::Ended)
            .filter(own)
        {
            let line = line(id, r.run.clone(), record_view(r, false));
            return Ok(Done::Done {
                line,
                files: Vec::new(),
            });
        }
        if let Some(mut r) = record
            .clone()
            .filter(|r| r.state == State::Laptop)
            .filter(|r| own(&r))
        {
            let marker = CancelMarker {
                by: agent.to_string(),
                replaced_by: None,
            };
            sups.cancel(store, &lock, &mut r, &marker)?;
            let line = line(id, r.run.clone(), record_view(&r, false));
            return Ok(Done::Done {
                line,
                files: Vec::new(),
            });
        }
        // An earlier laptop run the agent follows has ended: its own result.
        if let Some(done) = run.as_deref().map(|run| ended_laptop_run(store, id, run)) {
            if let Some(done) = done? {
                return Ok(done);
            }
        }
        (
            run.or_else(|| record.as_ref().and_then(|r| r.run.clone())),
            record.map(|r| r.generation),
        )
    };
    let req = CancelRequest {
        id: id.to_owned(),
        run,
        by: agent.to_string(),
    };
    let job = match remote.cancel(&req) {
        Ok(job) => job,
        Err(CallError::Unreachable(why)) => {
            return Ok(Done::Refused(format!("the VM is unreachable: {why}")))
        }
        Err(e) => return Err(Error::Preflight(e)),
    };
    if job.state == VmState::Unknown && generation.is_none() {
        return Ok(Done::Refused(
            "neither the laptop nor the VM knows it".to_owned(),
        ));
    }
    let lock = store.placement_lock()?;
    // The reply is about the run cancelled; the record follows the id, so it
    // learns from it only when that is the record's own run.
    let own = |r: &JobRecord| Some(r.generation) == generation && r.run == job.run;
    if let Some(mut r) = store.record(id)?.filter(own) {
        let reply = PreflightReply {
            jobs: [(id.to_owned(), job.clone())].into(),
            ..PreflightReply::default()
        };
        let change = refresh(&r, store.job_lock_held(id)?, Some(&reply));
        snapshot::apply(store, &lock, &mut r, change, Some(&reply))?;
    }
    Ok(Done::Done {
        line: line(id, job.run.clone(), vm_view(&job)),
        files: Vec::new(),
    })
}

/// Resume the paused VM run of `id` the agent follows from its kept
/// checkpoint, asking for `memory_mb` and capped at `timeout_secs` when
/// given: its next attempt goes first in the queue.
///
/// # Errors
/// If the laptop's state cannot be read, or the VM call connects and fails.
pub fn resume(
    store: &Store,
    remote: &mut impl Remote,
    agent: &Label,
    id: &str,
    memory_mb: Option<u64>,
    timeout_secs: Option<u64>,
) -> Result<Done, Error> {
    let (run, record) = target(store, agent, id)?;
    let run = match run {
        Ok(run) => run,
        Err(done) => return Ok(done),
    };
    let req = ResumeRequest {
        id: id.to_owned(),
        run: run.or_else(|| record.and_then(|r| r.run)),
        memory_mb,
        timeout_secs,
    };
    let reply = match remote.resume(&req) {
        Ok(reply) => reply,
        Err(CallError::Unreachable(why)) => {
            return Ok(Done::Refused(format!("the VM is unreachable: {why}")))
        }
        Err(e) => return Err(Error::Preflight(e)),
    };
    if let Some(why) = reply.refused {
        return Ok(Done::Refused(why));
    }
    Ok(Done::Done {
        line: line(id, reply.job.run.clone(), vm_view(&reply.job)),
        files: Vec::new(),
    })
}

/// Write the files of the run of `id` the agent follows under
/// `<out>/<id>/<run>/<attempt>/`. A run that never started writes nothing.
///
/// # Errors
/// If the laptop's state cannot be read, a file not written, or the VM call
/// connects and fails.
pub fn fetch(
    store: &Store,
    remote: &mut impl Remote,
    agent: &Label,
    id: &str,
    out: &Path,
) -> Result<Done, Error> {
    let (run, record) = target(store, agent, id)?;
    let run = match run {
        Ok(run) => run,
        Err(done) => return Ok(done),
    };
    if let Some(why) = uncertain(record.as_ref(), store.job_lock_held(id)?) {
        return Ok(Done::Refused(why));
    }
    let run = run.or_else(|| record.as_ref().and_then(|r| r.run.clone()));
    let mut files = Vec::new();
    if let Some(run) = run.as_deref() {
        let dir = store.run_dir(id, run);
        // A laptop run is attempt 0; a moved run's VM attempts follow it.
        for name in ["tlc.log", "result.json"] {
            let from = dir.join(name);
            if from.exists() {
                let to = out.join(id).join(run).join("0");
                std::fs::create_dir_all(&to)?;
                let to = to.join(name);
                std::fs::copy(&from, &to)?;
                files.push(to);
            }
        }
        let own = record.as_ref().filter(|r| r.run.as_deref() == Some(run));
        let on_vm = own.is_some_and(|r| match r.state {
            State::Vm | State::Moving => true,
            State::Ended => r.result.as_ref().is_some_and(|x| x.place != "laptop"),
            State::Laptop | State::Sent => false,
        });
        if dir.exists() && !on_vm {
            let view = own.map_or(View::Unknown, |r| record_view(r, false));
            let mut lines = [line(id, Some(run.to_owned()), view)];
            from_run_dirs(store, &mut lines)?;
            let [line] = lines;
            return Ok(Done::Done { line, files });
        }
    }
    let req = FetchRequest {
        id: id.to_owned(),
        run,
    };
    let bytes = match remote.fetch(&req) {
        Ok(bytes) => bytes,
        Err(CallError::Unreachable(why)) => {
            return Ok(Done::Refused(format!("the VM is unreachable: {why}")))
        }
        Err(e) => return Err(Error::Preflight(e)),
    };
    let (reply, contents) =
        decode(&bytes).map_err(|why| Error::Preflight(CallError::Failed(why)))?;
    if reply.job.state == VmState::Unknown && record.is_none() {
        return Ok(Done::Refused(
            "neither the laptop nor the VM knows it".to_owned(),
        ));
    }
    let run = reply.job.run.clone().unwrap_or_default();
    for (f, bytes) in reply.files.iter().zip(contents) {
        let dir = out.join(id).join(&run).join(f.attempt.to_string());
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(&f.name);
        std::fs::write(&path, bytes)?;
        files.push(path);
    }
    let view = match (&reply.job.state, &record) {
        (VmState::Unknown, Some(r)) => record_view(r, false),
        _ => vm_view(&reply.job),
    };
    Ok(Done::Done {
        line: line(id, reply.job.run.clone(), view),
        files,
    })
}

/// The end of the laptop run `run` of `id`, from its directory, if it ran on
/// the laptop and ended.
fn ended_laptop_run(store: &Store, id: &str, run: &str) -> std::io::Result<Option<Done>> {
    let result: Option<JobResult> = read_json(&store.run_dir(id, run).join("result.json"))?;
    Ok(result.map(|r| Done::Done {
        line: line(id, Some(run.to_owned()), View::Ended(r)),
        files: Vec::new(),
    }))
}

/// A fetch's reply line and each file's bytes, as the runner streams them.
fn decode(bytes: &[u8]) -> Result<(FetchReply, Vec<&[u8]>), String> {
    let at = bytes
        .iter()
        .position(|b| *b == b'\n')
        .ok_or("no reply line")?;
    let (head, mut rest) = bytes.split_at(at);
    rest = rest.get(1..).unwrap_or_default();
    let reply: FetchReply = serde_json::from_slice(head).map_err(|e| format!("the reply: {e}"))?;
    let mut contents = Vec::new();
    for f in &reply.files {
        let len = usize::try_from(f.len).map_err(|e| e.to_string())?;
        let (file, more) = rest
            .split_at_checked(len)
            .ok_or("the files are cut short")?;
        contents.push(file);
        rest = more;
    }
    Ok((reply, contents))
}

/// A run's line, keyed by nothing.
fn line(id: &str, run: Option<String>, view: View) -> Line {
    Line {
        key: None,
        id: id.to_owned(),
        run,
        view,
        eta: None,
        engine: None,
    }
}

#[cfg(test)]
#[path = "manage_tests.rs"]
mod tests;

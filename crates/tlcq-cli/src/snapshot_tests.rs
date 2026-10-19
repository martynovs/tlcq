//! Tests of the preflight's snapshot and refresh.

use std::collections::BTreeMap;

use super::*;
use crate::commit::CommitReply;
use crate::proto::{CommitAnswer, JobInfo, TokenAnswer, VmJob, VmState};
use crate::runner::state::tests::{entry, info};

/// A VM that answers the preflight with a canned reply.
struct Canned {
    reply: Result<PreflightReply, CallError>,
    seen: Option<PreflightRequest>,
}

impl Remote for Canned {
    fn preflight(&mut self, req: &PreflightRequest) -> Result<PreflightReply, CallError> {
        self.seen = Some(req.clone());
        self.reply.clone()
    }

    fn status(&mut self, req: &PreflightRequest) -> Result<PreflightReply, CallError> {
        self.preflight(req)
    }

    fn commit(&mut self, _: &[u8]) -> Result<CommitReply, CallError> {
        Err(CallError::Failed("no commits here".to_owned()))
    }
    fn ls(&mut self) -> Result<std::collections::BTreeMap<String, crate::proto::VmJob>, CallError> {
        Err(CallError::Failed("cut".to_owned()))
    }

    fn cancel(
        &mut self,
        _: &crate::proto::CancelRequest,
    ) -> Result<crate::proto::VmJob, CallError> {
        Err(CallError::Failed("cut".to_owned()))
    }

    fn resume(
        &mut self,
        _: &crate::proto::ResumeRequest,
    ) -> Result<crate::proto::ResumeReply, CallError> {
        Err(CallError::Failed("cut".to_owned()))
    }

    fn fetch(&mut self, _: &crate::proto::FetchRequest) -> Result<Vec<u8>, CallError> {
        Err(CallError::Failed("cut".to_owned()))
    }
}

fn canned(reply: Result<PreflightReply, CallError>) -> Canned {
    Canned { reply, seen: None }
}

fn vm_job(state: VmState, run: &str) -> VmJob {
    VmJob {
        state,
        run: Some(run.to_owned()),
        entries: vec![entry("A.cfg")],
        place: None,
        elapsed_secs: None,
        counters: None,
        lanes: Vec::new(),
        flags: crate::proto::Flags::default(),
        curve: None,
        position: None,
        left_secs: None,
        cancelled: None,
        started_at: None,
        expected: None,
        waits_for: None,
        held: None,
    }
}

fn record(
    store: &Store,
    lock: &PlacementLock,
    id: &str,
    state: State,
    token: Option<&str>,
) -> JobRecord {
    let mut r = JobRecord::new(id, state);
    r.token = token.map(str::to_owned);
    r.info = Some(info());
    store.write_record(lock, &mut r).unwrap();
    r
}

fn take_with(
    store: &Store,
    lock: &PlacementLock,
    vm: &mut Canned,
    ids: &[&str],
) -> Result<Snapshot, Error> {
    let ids: Vec<String> = ids.iter().map(|s| (*s).to_owned()).collect();
    take(
        store,
        lock,
        vm,
        &crate::engine::test_versions("J"),
        &ids,
        Vec::new(),
        2,
    )
}

#[test]
fn the_request_holds_every_unresolved_id_and_token() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lock = store.placement_lock().unwrap();
    record(&store, &lock, "vm", State::Vm, None);
    record(&store, &lock, "sent", State::Sent, Some("t1"));
    record(&store, &lock, "moving", State::Moving, Some("t2"));
    record(&store, &lock, "laptop", State::Laptop, None);
    record(&store, &lock, "ended", State::Ended, None);
    let follow = Follow {
        number: 1,
        key: entry("A.cfg").key,
        label: entry("A.cfg").label,
        id: "x".to_owned(),
        what: Followed::Run(RunRef::Token("t3".to_owned())),
    };
    store.write_follow(&lock, &follow).unwrap();
    let mut vm = canned(Err(CallError::Unreachable("down".to_owned())));
    let snap = take_with(&store, &lock, &mut vm, &["new"]).unwrap();
    let req = vm.seen.unwrap();
    assert_eq!(req.ids, ["moving", "new", "sent", "vm"]);
    assert_eq!(req.tokens, ["t1", "t2", "t3"]);
    assert!(snap.vm.is_none());
    assert_eq!(snap.laptop.records["sent"].state, State::Sent);
    assert_eq!(snap.laptop.free_slots, 2);
}

#[test]
fn a_preflight_that_fails_after_connecting_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lock = store.placement_lock().unwrap();
    let mut vm = canned(Err(CallError::Failed("cut".to_owned())));
    assert!(matches!(
        take_with(&store, &lock, &mut vm, &[]),
        Err(Error::Preflight(_))
    ));
}

#[test]
fn records_and_follows_are_refreshed_from_the_reply() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lock = store.placement_lock().unwrap();
    record(&store, &lock, "sent", State::Sent, Some("t1"));
    let mut done = record(&store, &lock, "done", State::Vm, None);
    done.run = Some("v2".to_owned());
    store.write_record(&lock, &mut done).unwrap();
    record(&store, &lock, "lost", State::Sent, Some("t2"));
    let follow = Follow {
        number: 4,
        key: entry("A.cfg").key,
        label: entry("A.cfg").label,
        id: "sent".to_owned(),
        what: Followed::Run(RunRef::Token("t1".to_owned())),
    };
    store.write_follow(&lock, &follow).unwrap();
    let ok = crate::runner::state::State::outcome_only(Outcome::Ok);
    let mut reply = PreflightReply {
        jars_match: true,
        ..PreflightReply::default()
    };
    reply
        .jobs
        .insert("sent".to_owned(), vm_job(VmState::Running, "v1"));
    reply.jobs.insert(
        "done".to_owned(),
        vm_job(VmState::Ended { result: ok.clone() }, "v2"),
    );
    let reached = TokenAnswer::Reached {
        run: "v1".to_owned(),
    };
    reply.tokens.insert(
        "t1".to_owned(),
        CommitAnswer::Applied {
            jobs: BTreeMap::from([("sent".to_owned(), reached)]),
        },
    );
    reply
        .tokens
        .insert("t2".to_owned(), CommitAnswer::NotApplied);
    let mut vm = canned(Ok(reply));
    let snap = take_with(&store, &lock, &mut vm, &["done"]).unwrap();
    let sent = &snap.laptop.records["sent"];
    assert_eq!(
        (sent.state, sent.run.as_deref(), sent.token.as_deref()),
        (State::Vm, Some("v1"), None)
    );
    assert_eq!(sent.mirror, [entry("A.cfg")]);
    assert_eq!(snap.laptop.records["done"].state, State::Ended);
    assert_eq!(snap.laptop.cache.get("done"), Some(&ok));
    let lost = &snap.laptop.records["lost"];
    assert_eq!(
        lost.result.as_ref().map(|r| &r.outcome),
        Some(&Outcome::NeverSent)
    );
    let history = History::load(&store.history()).unwrap();
    let ends: Vec<(&str, &str)> = history
        .records()
        .iter()
        .map(|r| (r.id.as_str(), r.end.as_str()))
        .collect();
    assert_eq!(ends, [("done", "v2")]);
    let follows = store.follows().unwrap();
    assert_eq!(follows[0].what, Followed::Run(RunRef::Run("v1".to_owned())));
}

#[test]
fn a_laptop_job_with_a_live_supervisor_holds_a_slot() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lock = store.placement_lock().unwrap();
    record(&store, &lock, "busy", State::Laptop, None);
    let held = std::fs::File::create(store.job_dir("busy").join("lock")).unwrap();
    held.lock().unwrap();
    let mut vm = canned(Ok(PreflightReply::default()));
    // Waiting for the slot: it holds one, but does not run.
    let snap = take_with(&store, &lock, &mut vm, &[]).unwrap();
    assert_eq!(snap.laptop.free_slots, 1);
    assert!(!snap.laptop.running.contains("busy") && snap.laptop.locked.contains("busy"));
    let run_dir = store.run_dir("busy", "");
    std::fs::create_dir_all(&run_dir).unwrap();
    let running = crate::laptop_runs::Progress {
        phase: crate::laptop_runs::Phase::Running,
        ..Default::default()
    };
    crate::files::write_json(&run_dir.join("progress.json"), &running).unwrap();
    let mut vm = canned(Ok(PreflightReply::default()));
    let snap = take_with(&store, &lock, &mut vm, &[]).unwrap();
    assert_eq!(snap.laptop.free_slots, 1);
    assert!(snap.laptop.running.contains("busy"));
    // A move runs no TLC: it counts as running, and frees the slot.
    record(&store, &lock, "busy", State::Moving, Some("t"));
    let mut vm = canned(Ok(PreflightReply::default()));
    let snap = take_with(&store, &lock, &mut vm, &[]).unwrap();
    assert_eq!(snap.laptop.free_slots, 2);
    assert!(snap.laptop.running.contains("busy"));
}

#[test]
fn an_end_without_a_run_or_a_description_writes_no_history() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lock = store.placement_lock().unwrap();
    let mut r = JobRecord::new("a", State::Vm);
    let error = crate::runner::state::State::outcome_only(Outcome::Error { reason: None });
    end(&store, &lock, &mut r, &error).unwrap();
    r.run = Some("v1".to_owned());
    end(&store, &lock, &mut r, &error).unwrap();
    r.info = Some(JobInfo { ..info() });
    for outcome in [
        Outcome::Timeout,
        Outcome::Cancelled {
            by: "x".to_owned(),
            replaced_by: None,
        },
        Outcome::Unexpected,
    ] {
        r.run = Some(format!("v{}", outcome_name(&outcome).unwrap().len()));
        let result = crate::runner::state::State::outcome_only(outcome);
        end(&store, &lock, &mut r, &result).unwrap();
    }
    let history = History::load(&store.history()).unwrap();
    let outcomes: Vec<&str> = history
        .records()
        .iter()
        .map(|r| r.outcome.as_str())
        .collect();
    assert_eq!(outcomes, ["timeout", "cancelled", "unexpected"]);
    assert_eq!(
        outcome_name(&Outcome::Error { reason: None }),
        Some("error")
    );
    assert_eq!(outcome_name(&Outcome::NeverSent), None);
}

#[test]
fn an_earlier_run_s_end_is_recorded_once_it_ends_and_dropped_if_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lock = store.placement_lock().unwrap();
    let mut r = record(&store, &lock, "a", State::Vm, None);
    r.run = Some("v4".to_owned());
    // v0 is not asked about by this reply: it is kept, to be asked next.
    r.ending = ["v0", "v1", "v2", "v3"].map(str::to_owned).to_vec();
    let ask = |run: &str| RunAsk {
        id: "a".to_owned(),
        run: run.to_owned(),
    };
    let ended = crate::runner::state::State::outcome_only(Outcome::Timeout);
    let reply = PreflightReply {
        runs: vec![
            (ask("v1"), vm_job(VmState::Stopping, "v1")),
            (ask("v2"), vm_job(VmState::Unknown, "v2")),
            (ask("v3"), vm_job(VmState::Ended { result: ended }, "v3")),
        ],
        ..PreflightReply::default()
    };
    ended_runs(&store, &lock, &mut r, &reply).unwrap();
    assert_eq!(r.ending, ["v0", "v1"]);
    let history = History::load(&store.history()).unwrap();
    let ends: Vec<&str> = history.records().iter().map(|h| h.end.as_str()).collect();
    assert_eq!(ends, ["v3"]);
    let mut none = JobRecord::new("b", State::Vm);
    ended_runs(&store, &lock, &mut none, &reply).unwrap();
    assert_eq!(store.record("b").unwrap(), None);
}

#[test]
fn a_record_that_moves_to_another_run_keeps_asking_about_the_one_it_left() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lock = store.placement_lock().unwrap();
    let mut r = record(&store, &lock, "a", State::Vm, None);
    r.run = Some("v1".to_owned());
    let mut reply = PreflightReply::default();
    reply
        .jobs
        .insert("a".to_owned(), vm_job(VmState::Queued, "v2"));
    apply(&store, &lock, &mut r, Change::Vm, Some(&reply)).unwrap();
    assert_eq!(
        (r.run.as_deref(), r.ending.clone()),
        (Some("v2"), vec!["v1".to_owned()])
    );
    let ok = crate::runner::state::State::outcome_only(Outcome::Ok);
    reply.jobs.insert(
        "a".to_owned(),
        vm_job(VmState::Ended { result: ok.clone() }, "v3"),
    );
    apply(&store, &lock, &mut r, Change::End(ok), Some(&reply)).unwrap();
    assert_eq!(
        (r.run.as_deref(), r.ending.clone()),
        (Some("v3"), vec!["v1".to_owned(), "v2".to_owned()])
    );
    // The same run again changes nothing.
    r.take_run(Some("v3".to_owned()));
    assert_eq!(r.ending.len(), 2);
}

#[test]
fn a_vm_run_s_curve_is_kept_with_its_end_in_the_history() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lock = store.placement_lock().unwrap();
    let mut r = record(&store, &lock, "a", State::Vm, None);
    r.run = Some("v1".to_owned());
    r.ending = vec!["v0".to_owned()];
    let ok = crate::runner::state::State::outcome_only(Outcome::Ok);
    let curve = |attempt| {
        Some(crate::proto::AttemptCurve {
            attempt,
            text: format!("{{\"kind\":\"end\",\"secs\":{attempt}}}\n"),
        })
    };
    let ended = |run: &str, attempt| VmJob {
        curve: curve(attempt),
        ..vm_job(VmState::Ended { result: ok.clone() }, run)
    };
    let mut reply = PreflightReply::default();
    reply.jobs.insert("a".to_owned(), ended("v1", 2));
    reply.runs.push((
        RunAsk {
            id: "a".to_owned(),
            run: "v0".to_owned(),
        },
        ended("v0", 1),
    ));
    ended_runs(&store, &lock, &mut r, &reply).unwrap();
    apply(&store, &lock, &mut r, Change::End(ok.clone()), Some(&reply)).unwrap();
    assert!(store.curve_path("a", "v1", 2).exists() && store.curve_path("a", "v0", 1).exists());
    let history = History::load(&store.history()).unwrap();
    let curves: Vec<&[String]> = history
        .records()
        .iter()
        .map(|h| h.curves.as_slice())
        .collect();
    assert_eq!(
        curves,
        [["a-v0-1.jsonl".to_owned()], ["a-v1-2.jsonl".to_owned()]]
    );
}

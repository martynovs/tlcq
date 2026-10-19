//! Tests of the verbs against a state directory and fake containers.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::commit::tests::contents;
use crate::commit::{CommitJob, CommitRequest, CommitRoute, JobAnswer, SupersedeOrder};
use crate::outcome::Outcome;
use crate::proto::{FetchReply, FetchRequest, Held};
use crate::runner::admit::Move;
use crate::runner::state::tests::{entry, info, key};
use crate::runner::state::{Phase, Slot};

/// Containers that start unless told to fail, and run until told they are gone.
#[derive(Debug, Clone, Default)]
pub(crate) struct Fake(pub(crate) Rc<RefCell<FakeState>>);

#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per way the fake fails"
)]
#[derive(Debug, Default)]
pub(crate) struct FakeState {
    pub(crate) running: BTreeSet<String>,
    pub(crate) fail: bool,
    /// Too little memory free for any new attempt.
    pub(crate) full: bool,
    pub(crate) started: Vec<String>,
    pub(crate) stopped: Vec<String>,
    pub(crate) removed: Vec<String>,
    /// Image prunes asked for.
    pub(crate) pruned: u32,
    /// Paused containers.
    pub(crate) paused: BTreeSet<String>,
    /// Each container's CPUs, as the slot it was last started in or moved to.
    pub(crate) slots: BTreeMap<String, Slot>,
    /// Pauses, unpauses and moves fail.
    pub(crate) stuck: bool,
    /// Only unpauses fail.
    pub(crate) no_unpause: bool,
}

impl Containers for Fake {
    fn paused(&self) -> std::io::Result<BTreeSet<String>> {
        Ok(self.0.borrow().paused.clone())
    }

    fn running(&self) -> std::io::Result<BTreeSet<String>> {
        Ok(self.0.borrow().running.clone())
    }

    /// Memory for a new attempt unless `full`; with `full`, only once a
    /// frozen container's limit is not promised.
    fn memory(&self, _: Slot, _: bool, frozen: &BTreeSet<String>) -> Option<u64> {
        let s = self.0.borrow();
        (!s.full || !frozen.is_empty()).then_some(8192)
    }

    fn shares(&self) -> crate::runner::admit::Shares {
        crate::runner::admit::Shares {
            slot_mb: 8192,
            whole_mb: 16_384,
        }
    }

    fn workers(&self, _: Slot) -> u64 {
        16
    }

    fn free_mb(&self) -> Option<u64> {
        Some(8192)
    }

    fn room_mb(&self) -> Option<u64> {
        Some(6144)
    }

    fn pause(&mut self, name: &str) -> Result<(), String> {
        let mut s = self.0.borrow_mut();
        if s.stuck {
            return Err("podman said no".to_owned());
        }
        s.paused.insert(name.to_owned());
        Ok(())
    }

    fn unpause(&mut self, name: &str) -> Result<(), String> {
        let mut s = self.0.borrow_mut();
        if s.stuck || s.no_unpause {
            return Err("podman said no".to_owned());
        }
        s.paused.remove(name);
        Ok(())
    }

    fn move_to(&mut self, name: &str, slot: Slot) -> Result<(), String> {
        let mut s = self.0.borrow_mut();
        if s.stuck {
            return Err("podman said no".to_owned());
        }
        s.slots.insert(name.to_owned(), slot);
        Ok(())
    }

    fn start(&mut self, name: &str, _: &str, _: &Run, a: &Attempt, _: u64) -> Result<(), String> {
        let mut s = self.0.borrow_mut();
        if s.fail {
            return Err("podman said no".to_owned());
        }
        s.running.insert(name.to_owned());
        s.started.push(name.to_owned());
        s.slots.insert(name.to_owned(), a.slot);
        Ok(())
    }

    fn stop(&mut self, name: &str) {
        self.0.borrow_mut().stopped.push(name.to_owned());
    }

    fn remove(&mut self, name: &str) {
        self.0.borrow_mut().removed.push(name.to_owned());
    }

    fn prune_images(&mut self) {
        self.0.borrow_mut().pruned += 1;
    }
}

impl Fake {
    pub(crate) fn gone(&self, name: &str) {
        self.0.borrow_mut().running.remove(name);
    }
}

pub(crate) fn runner(dir: &std::path::Path) -> (Runner<Fake>, Fake) {
    let fake = Fake::default();
    let runner = Runner {
        store: Store::open(dir).unwrap(),
        containers: fake.clone(),
        versions: crate::engine::test_versions("J"),
        limits: Limits::default(),
        never_started: 3,
        disk_floor: 0,
    };
    // One slot, as most tests want; those of two slots open P.
    runner.store.set_p_open(false).unwrap();
    (runner, fake)
}

/// A job of spec `spec`, its id computed with jar `J`.
pub(crate) fn job(spec: &str, route: CommitRoute) -> (CommitJob, Vec<u8>) {
    let c = contents();
    let mut info = info();
    info.spec = spec.to_owned();
    info.entries = vec![entry(&format!("{spec}.cfg"))];
    let id = crate::id::id(&crate::id::manifest(
        &info.spec,
        &c,
        &info.tlc_opts,
        "tlc J",
    ));
    let tar = c.tarball().unwrap();
    let tar_len = tar.len() as u64;
    (
        CommitJob {
            id,
            info,
            route,
            tar_len,
        },
        tar,
    )
}

pub(crate) fn wire(
    token: &str,
    jobs: Vec<(CommitJob, Vec<u8>)>,
    supersede: Vec<SupersedeOrder>,
) -> Vec<u8> {
    let (jobs, tars): (Vec<_>, Vec<_>) = jobs.into_iter().unzip();
    let req = CommitRequest {
        versions: crate::engine::test_versions("J"),
        token: token.to_owned(),
        pending: Vec::new(),
        jobs,
        supersede,
    };
    commit::encode(&req, &tars).unwrap()
}

fn answer(reply: &CommitReply, id: &str) -> JobAnswer {
    match reply {
        CommitReply::Applied { jobs, .. } => jobs[id].clone(),
        other => panic!("not applied: {other:?}"),
    }
}

fn state(r: &Runner<Fake>) -> State {
    let lock = r.store.lock().unwrap();
    r.store.load(&lock).unwrap()
}

#[test]
fn a_commit_stores_queues_and_starts_its_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let (a, tar_a) = job("A", CommitRoute::NowOrRefuse);
    let (b, tar_b) = job("B", CommitRoute::Queue);
    let (ida, idb) = (a.id.clone(), b.id.clone());
    let reply = r
        .commit(wire("t1", vec![(a, tar_a), (b, tar_b)], Vec::new()).as_slice())
        .unwrap();
    let started = answer(&reply, &ida);
    assert_eq!(
        started,
        JobAnswer::Started {
            run: "v1".to_owned()
        }
    );
    assert_eq!(
        answer(&reply, &idb),
        JobAnswer::Queued {
            run: "v1".to_owned()
        }
    );
    assert_eq!(fake.0.borrow().started, [container_name(&ida, "v1", 1)]);
    assert!(r.store.tarball(&idb).exists());
    assert!(r
        .store
        .attempt_dir(&ida, "v1", 1)
        .join("result.json")
        .exists());
    let s = state(&r);
    assert!(matches!(
        s.latest(&ida).unwrap().phase,
        Phase::Running { .. }
    ));
    assert_eq!(s.latest(&idb).unwrap().phase, Phase::Queued);
}

#[test]
fn a_job_that_fails_the_checks_makes_the_commit_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _) = runner(dir.path());
    r.versions = crate::engine::test_versions("K");
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    let reply = r
        .commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    assert!(matches!(reply, CommitReply::Invalid { id: ref i, .. } if *i == id));
    // Nothing applied, and its token says so (D6, Tokens).
    let s = state(&r);
    assert!(s.runs.is_empty() && s.queue.is_empty());
    assert_eq!(
        s.tokens.get("t1"),
        Some(&crate::proto::CommitAnswer::NotApplied)
    );
    r.commit(&b"{"[..]).unwrap_err();
}

#[test]
fn a_request_above_what_the_vm_can_give_makes_the_commit_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _) = runner(dir.path());
    // The fake VM gives at most 16384 MB.
    let (mut a, tar) = job("A", CommitRoute::Queue);
    a.info.memory_mb = Some(16_385);
    let reply = r
        .commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    assert!(
        matches!(&reply, CommitReply::Invalid { reason, .. } if reason.contains("more than the 16384 MB")),
        "{reply:?}"
    );
    assert!(state(&r).runs.is_empty());
    let (mut a, tar) = job("A", CommitRoute::Queue);
    a.info.memory_mb = Some(16_384);
    let reply = r
        .commit(wire("t2", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    assert!(!matches!(reply, CommitReply::Invalid { .. }), "{reply:?}");
}

#[test]
fn a_tombstoned_commit_stores_no_tarball() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _) = runner(dir.path());
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    r.preflight(&preflight(&[], &["t1"])).unwrap();
    let reply = r
        .commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    assert_eq!(reply, CommitReply::NotApplied);
    assert!(!r.store.tarball(&id).exists());
}

#[test]
fn a_failed_start_refuses_a_now_or_refuse_job() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    fake.0.borrow_mut().fail = true;
    let (a, tar) = job("A", CommitRoute::NowOrRefuse);
    let id = a.id.clone();
    let reply = r
        .commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    assert_eq!(answer(&reply, &id), JobAnswer::Refused);
    assert!(!r.store.attempt_dir(&id, "v1", 1).exists());
    assert!(!state(&r).runs.contains_key(&id));
}

#[test]
fn the_next_verb_settles_a_gone_attempt_and_admits_the_next_job() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let (a, tar_a) = job("A", CommitRoute::Queue);
    let (b, tar_b) = job("B", CommitRoute::Queue);
    let (ida, idb) = (a.id.clone(), b.id.clone());
    r.commit(wire("t1", vec![(a, tar_a), (b, tar_b)], Vec::new()).as_slice())
        .unwrap();
    let (first, second) = (&ida, &idb);
    let attempt = r.store.attempt_dir(first, "v1", 1);
    std::fs::write(attempt.join("started"), "").unwrap();
    let verdict = AttemptResult {
        finished: Some(State::outcome_only(Outcome::Ok)),
        ..AttemptResult::default()
    };
    crate::files::write_json(&attempt.join("result.json"), &verdict).unwrap();
    fake.gone(&container_name(first, "v1", 1));
    let reply = r
        .commit(wire("t2", Vec::new(), Vec::new()).as_slice())
        .unwrap();
    assert!(matches!(reply, CommitReply::Applied { .. }));
    let s = state(&r);
    assert!(matches!(
        s.latest(first).unwrap().phase,
        Phase::Ended { .. }
    ));
    assert!(s.cache.contains_key(first.as_str()));
    assert!(!attempt.join("scratch").exists());
    assert_eq!(fake.0.borrow().removed, [container_name(first, "v1", 1)]);
    assert_eq!(
        fake.0.borrow().started.last(),
        Some(&container_name(second, "v1", 1))
    );
}

#[test]
fn an_attempt_gone_before_its_start_is_undone_and_retried() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    r.commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    fake.gone(&container_name(&id, "v1", 1));
    r.commit(wire("t2", Vec::new(), Vec::new()).as_slice())
        .unwrap();
    assert!(!r.store.attempt_dir(&id, "v1", 1).exists());
    assert_eq!(
        fake.0.borrow().started.last(),
        Some(&container_name(&id, "v1", 2))
    );
    let first = crate::files::read_json::<AttemptResult>(
        &r.store.attempt_dir(&id, "v1", 2).join("result.json"),
    );
    assert_eq!(first.unwrap(), Some(AttemptResult::default()));
}

#[test]
fn a_supersede_marks_and_stops_the_running_old_version() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let (old, tar_old) = job("A", CommitRoute::Queue);
    let old_id = old.id.clone();
    r.commit(wire("t1", vec![(old, tar_old)], Vec::new()).as_slice())
        .unwrap();
    let order = SupersedeOrder {
        old: old_id.clone(),
        key: key("A.cfg"),
        by: "new".to_owned(),
        label: "agent".to_owned(),
        attached: false,
    };
    r.commit(wire("t2", Vec::new(), vec![order]).as_slice())
        .unwrap();
    let marker = r.store.attempt_dir(&old_id, "v1", 1).join("cancel.json");
    assert!(marker.exists());
    assert_eq!(fake.0.borrow().stopped, [container_name(&old_id, "v1", 1)]);
    assert!(matches!(
        state(&r).latest(&old_id).unwrap().phase,
        Phase::Stopping { .. }
    ));
}

#[test]
fn a_stopping_runner_admits_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    r.store.set_stopping(true).unwrap();
    let (a, tar) = job("A", CommitRoute::Queue);
    r.commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    assert_eq!(fake.0.borrow().started, Vec::<String>::new());
    // Its next start clears the mark, and the queue is admitted.
    r.store.set_stopping(false).unwrap();
    r.store.set_stopping(false).unwrap();
    r.tick(true).unwrap();
    assert_eq!(fake.0.borrow().started.len(), 1);
    std::fs::create_dir(dir.path().join("stopping")).unwrap();
    r.store.set_stopping(false).unwrap_err();
}

#[test]
fn the_preflight_reports_a_gone_attempt_without_settling_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    r.commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    std::fs::write(r.store.attempt_dir(&id, "v1", 1).join("started"), "").unwrap();
    fake.gone(&container_name(&id, "v1", 1));
    let req = PreflightRequest {
        versions: crate::engine::test_versions("J"),
        ids: vec![id.clone()],
        keys: Vec::new(),
        tokens: vec!["t9".to_owned()],
        runs: Vec::new(),
    };
    let reply = r.preflight(&req).unwrap();
    assert!(matches!(
        reply.jobs[&id].state,
        crate::proto::VmState::Ended { .. }
    ));
    let s = state(&r);
    assert!(matches!(
        s.latest(&id).unwrap().phase,
        Phase::Running { .. }
    ));
    assert!(s.tokens.contains_key("t9"));
    assert!(r.store.attempt_dir(&id, "v1", 1).join("scratch").exists());
    r.preflight(&req).unwrap();
}

/// Commit job `spec`, let its attempt 1 stop for a pause with its checkpoint
/// kept and `so_far` its run, and settle it: the run is paused.
fn paused_job(r: &mut Runner<Fake>, fake: &Fake, spec: &str, so_far: &AttemptResult) -> String {
    let (a, tar) = job(spec, CommitRoute::Queue);
    let id = a.id.clone();
    r.commit(wire(&format!("{spec}1"), vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    let attempt = r.store.attempt_dir(&id, "v1", 1);
    std::fs::write(attempt.join("started"), "").unwrap();
    std::fs::write(attempt.join("paused"), "").unwrap();
    crate::files::write_json(&attempt.join("result.json"), so_far).unwrap();
    let at = r.store.checkpoint_dir(&id, "v1");
    std::fs::create_dir_all(at.join("tlc")).unwrap();
    std::fs::write(at.join("tlc/queue.chkpt"), "").unwrap();
    let kept = crate::runner::store::Checkpoint {
        workers: 16,
        fp: None,
        generated: None,
        attempt: Some(1),
    };
    crate::files::write_json(&at.join("checkpoint.json"), &kept).unwrap();
    fake.gone(&container_name(&id, "v1", 1));
    r.commit(wire(&format!("{spec}2"), Vec::new(), Vec::new()).as_slice())
        .unwrap();
    id
}

#[test]
fn a_resume_starts_its_next_attempt_from_the_run_so_far() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let so_far = AttemptResult {
        duration_secs: 900,
        vm_secs: 900,
        ..AttemptResult::default()
    };
    let id = paused_job(&mut r, &fake, "A", &so_far);
    let s = state(&r);
    assert!(s.latest(&id).unwrap().paused());
    assert_eq!(fake.0.borrow().started.len(), 1);
    // The checkpoint outlives settling; nothing starts until the resume.
    assert!(r.store.checkpoint(&id, "v1").unwrap().is_some());
    assert!(!r.resume("nope", "v1").unwrap());
    assert!(r.resume(&id, "v1").unwrap());
    assert_eq!(
        fake.0.borrow().started.last(),
        Some(&container_name(&id, "v1", 2))
    );
    let next = r.store.attempt_dir(&id, "v1", 2).join("result.json");
    assert_eq!(crate::files::read_json(&next).unwrap(), Some(so_far));
    let s = state(&r);
    let run = s.latest(&id).unwrap();
    assert_eq!(
        run.attempts.last().map(|a| (a.number, a.slot, a.resume)),
        Some((2, Slot::E, true))
    );
    // Not paused any more: a second resume does nothing.
    assert!(!r.resume(&id, "v1").unwrap());
}

#[test]
fn a_resumed_run_goes_first_in_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    r.store.set_p_open(true).unwrap();
    let p = paused_job(&mut r, &fake, "P", &AttemptResult::default());
    // Two jobs take both slots, a third waits.
    let mut ids = Vec::new();
    for spec in ["A", "B", "C"] {
        let (a, tar) = job(spec, CommitRoute::Queue);
        ids.push(a.id.clone());
        r.commit(wire(spec, vec![(a, tar)], Vec::new()).as_slice())
            .unwrap();
    }
    assert!(r.resume(&p, "v1").unwrap());
    assert_eq!(state(&r).queue[0], (p.clone(), "v1".to_owned()));
    // When A ends, the resume starts before C.
    std::fs::write(r.store.attempt_dir(&ids[0], "v1", 1).join("started"), "").unwrap();
    fake.gone(&container_name(&ids[0], "v1", 1));
    r.tick(false).unwrap();
    assert_eq!(
        fake.0.borrow().started.last(),
        Some(&container_name(&p, "v1", 2))
    );
    assert_eq!(state(&r).latest(&ids[2]).unwrap().phase, Phase::Queued);
}

#[test]
fn a_freeze_frees_the_slot_and_holds_the_job_frozen_until_an_unfreeze() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    r.store.set_p_open(true).unwrap();
    let mut ids = Vec::new();
    for spec in ["A", "B", "C"] {
        let (a, tar) = job(spec, CommitRoute::Queue);
        ids.push(a.id.clone());
        r.commit(wire(spec, vec![(a, tar)], Vec::new()).as_slice())
            .unwrap();
    }
    let name = |i: usize| container_name(&ids[i], "v1", 1);
    // Not started yet: nothing to freeze.
    assert!(!r.freeze(&ids[0], "v1").unwrap());
    std::fs::write(r.store.attempt_dir(&ids[0], "v1", 1).join("started"), "").unwrap();
    assert!(r.freeze(&ids[0], "v1").unwrap());
    assert!(fake.0.borrow().paused.contains(&name(0)));
    // C takes the E slot A left.
    assert_eq!(fake.0.borrow().started.last(), Some(&name(2)));
    assert_eq!(fake.0.borrow().slots[&name(2)], Slot::E);
    let a = state(&r).latest(&ids[0]).unwrap().clone();
    assert_eq!(
        a.held(),
        Some(Held::Frozen {
            since: a.attempts[0].frozen_at,
            spare: false
        })
    );
    let log = r.store.frozen_log(&ids[0], "v1", 1).unwrap();
    assert!(log.ends_with(" freeze\n"));
    // B ends: A, frozen by a human, stays frozen; P stays empty.
    std::fs::write(r.store.attempt_dir(&ids[1], "v1", 1).join("started"), "").unwrap();
    fake.gone(&name(1));
    r.tick(false).unwrap();
    assert!(fake.0.borrow().paused.contains(&name(0)));
    let a = state(&r)
        .latest(&ids[0])
        .unwrap()
        .live_attempt()
        .unwrap()
        .clone();
    assert_eq!((a.freeze, a.held), (Freeze::Frozen, true));
    // Not held (as after admission froze it for new work): it thaws into P
    // on spare capacity, and D, arriving, freezes it again.
    unhold(&r, &ids[0]);
    r.tick(false).unwrap();
    assert_eq!(fake.0.borrow().slots[&name(0)], Slot::P);
    assert!(!fake.0.borrow().paused.contains(&name(0)));
    let (d, tar) = job("D", CommitRoute::Queue);
    let d_id = d.id.clone();
    r.commit(wire("D", vec![(d, tar)], Vec::new()).as_slice())
        .unwrap();
    assert!(fake.0.borrow().paused.contains(&name(0)));
    assert_eq!(
        fake.0.borrow().started.last(),
        Some(&container_name(&d_id, "v1", 1))
    );
    // Unfrozen, it goes before the queue: when D ends it takes P.
    assert!(r.unfreeze(&ids[0], "v1").unwrap());
    assert!(!r.unfreeze(&ids[0], "v1").unwrap());
    std::fs::write(r.store.attempt_dir(&d_id, "v1", 1).join("started"), "").unwrap();
    fake.gone(&container_name(&d_id, "v1", 1));
    r.tick(false).unwrap();
    let s = state(&r);
    let a = s.latest(&ids[0]).unwrap().live_attempt().unwrap().clone();
    assert_eq!((a.freeze, a.slot), (Freeze::Running, Slot::P));
}

/// Queue a job "B" behind the running one: it takes the slot a freeze frees,
/// so the frozen job stays frozen rather than thawing back into it.
pub(crate) fn queue_behind(r: &mut Runner<Fake>) {
    let (b, tar) = job("B", CommitRoute::Queue);
    r.commit(wire("B", vec![(b, tar)], Vec::new()).as_slice())
        .unwrap();
}

#[test]
fn a_pause_asks_the_supervisor_and_unpauses_a_frozen_job() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    r.commit(wire("A", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    let dir1 = r.store.attempt_dir(&id, "v1", 1);
    std::fs::write(dir1.join("started"), "").unwrap();
    // A queued job takes the slot its freeze frees: it stays frozen.
    queue_behind(&mut r);
    assert!(r.freeze(&id, "v1").unwrap());
    // Once its search ended, none would land: refused, the job left frozen.
    std::fs::write(dir1.join("searched"), "").unwrap();
    assert!(!r.pause(&id, "v1").unwrap());
    assert!(!dir1.join("pause").exists());
    std::fs::remove_file(dir1.join("searched")).unwrap();
    assert!(r.pause(&id, "v1").unwrap());
    assert!(dir1.join("pause").exists());
    assert!(fake.0.borrow().paused.is_empty());
    let s = state(&r);
    let a = s.latest(&id).unwrap().live_attempt().unwrap().clone();
    assert_eq!((a.freeze, a.pausing), (Freeze::Unpaused, true));
    assert_eq!(s.latest(&id).unwrap().held(), Some(Held::Pausing));
    assert!(r
        .store
        .frozen_log(&id, "v1", 1)
        .unwrap()
        .ends_with(" unpause\n"));
    // Pausing: neither frozen, unfrozen nor paused again.
    assert!(!r.pause(&id, "v1").unwrap());
    assert!(!r.freeze(&id, "v1").unwrap());
    assert!(!r.unfreeze(&id, "v1").unwrap());
}

#[test]
fn a_stuck_container_freezes_and_pauses_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    r.commit(wire("A", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    std::fs::write(r.store.attempt_dir(&id, "v1", 1).join("started"), "").unwrap();
    fake.0.borrow_mut().stuck = true;
    assert!(!r.freeze(&id, "v1").unwrap());
    fake.0.borrow_mut().stuck = false;
    queue_behind(&mut r);
    assert!(r.freeze(&id, "v1").unwrap());
    fake.0.borrow_mut().stuck = true;
    r.pause(&id, "v1").unwrap_err();
    assert!(r
        .store
        .frozen_log(&id, "v1", 1)
        .unwrap()
        .ends_with(" freeze\n"));
    // Undone whole: no pause request left behind.
    assert!(!r.store.attempt_dir(&id, "v1", 1).join("pause").exists());
}

fn preflight(ids: &[&str], tokens: &[&str]) -> PreflightRequest {
    PreflightRequest {
        versions: crate::engine::test_versions("J"),
        ids: ids.iter().map(|s| (*s).to_owned()).collect(),
        keys: Vec::new(),
        tokens: tokens.iter().map(|s| (*s).to_owned()).collect(),
        runs: Vec::new(),
    }
}

#[test]
fn a_poll_settles_admits_and_tombstones() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let (a, tar_a) = job("A", CommitRoute::Queue);
    let (b, tar_b) = job("B", CommitRoute::Queue);
    let (ida, idb) = (a.id.clone(), b.id.clone());
    r.commit(wire("t1", vec![(a, tar_a), (b, tar_b)], Vec::new()).as_slice())
        .unwrap();
    let attempt = r.store.attempt_dir(&ida, "v1", 1);
    std::fs::write(attempt.join("started"), "").unwrap();
    fake.gone(&container_name(&ida, "v1", 1));
    let reply = r.status(&preflight(&[&ida, &idb], &["t9"])).unwrap();
    assert!(matches!(
        reply.jobs[&ida].state,
        crate::proto::VmState::Ended { .. }
    ));
    assert_eq!(reply.jobs[&idb].state, crate::proto::VmState::Running);
    assert!(state(&r).tokens.contains_key("t9"));
}

#[test]
fn cancel_ends_a_queued_run_and_stops_a_running_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let (a, tar_a) = job("A", CommitRoute::Queue);
    let (b, tar_b) = job("B", CommitRoute::Queue);
    let (ida, idb) = (a.id.clone(), b.id.clone());
    r.commit(wire("t1", vec![(a, tar_a), (b, tar_b)], Vec::new()).as_slice())
        .unwrap();
    let cancel = |id: &str| CancelRequest {
        id: id.to_owned(),
        run: None,
        by: "agent".to_owned(),
    };
    let queued = r.cancel(&cancel(&idb)).unwrap();
    assert!(matches!(queued.state, crate::proto::VmState::Ended { .. }));
    let running = r.cancel(&cancel(&ida)).unwrap();
    assert_eq!(running.state, crate::proto::VmState::Stopping);
    assert!(r
        .store
        .attempt_dir(&ida, "v1", 1)
        .join("cancel.json")
        .exists());
    assert_eq!(fake.0.borrow().stopped, [container_name(&ida, "v1", 1)]);
    let unknown = r.cancel(&cancel("nope")).unwrap();
    assert_eq!(unknown.state, crate::proto::VmState::Unknown);
    r.tick(false).unwrap();
    assert_eq!(fake.0.borrow().stopped.len(), 1);
    r.tick(true).unwrap();
    assert_eq!(fake.0.borrow().stopped.len(), 2);
    fake.gone(&container_name(&ida, "v1", 1));
    r.tick(true).unwrap();
    let s = state(&r);
    let ended = &s.latest(&ida).unwrap().phase;
    assert!(
        matches!(ended, Phase::Ended { result } if matches!(result.outcome, Outcome::Cancelled { .. }))
    );
}

#[test]
fn below_the_disk_floor_admission_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    r.disk_floor = u64::MAX;
    let (a, tar) = job("A", CommitRoute::Queue);
    r.commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    assert_eq!(fake.0.borrow().started, Vec::<String>::new());
}

#[test]
fn a_reorder_moves_a_queued_run_and_ls_lists_every_job() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _) = runner(dir.path());
    let jobs: Vec<_> = ["A", "B", "C"]
        .iter()
        .map(|s| job(s, CommitRoute::Queue))
        .collect();
    let ids: Vec<String> = jobs.iter().map(|(j, _)| j.id.clone()).collect();
    r.commit(wire("t1", jobs, Vec::new()).as_slice()).unwrap();
    assert!(r.reorder(&ids[2], "v1", Move::Head).unwrap());
    let queue: Vec<String> = state(&r).queue.into_iter().map(|(i, _)| i).collect();
    assert_eq!(queue, [ids[0].clone(), ids[2].clone(), ids[1].clone()]);
    let ls = r.ls().unwrap();
    assert_eq!(ls.len(), 3);
    assert_eq!(ls[&ids[0]].state, crate::proto::VmState::Running);
    assert_eq!(ls[&ids[2]].state, crate::proto::VmState::Queued);
}

#[test]
fn fetch_streams_each_recorded_attempt_s_files_after_its_reply() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _) = runner(dir.path());
    let (a, tar_a) = job("A", CommitRoute::Queue);
    let (b, tar_b) = job("B", CommitRoute::Queue);
    let (ida, idb) = (a.id.clone(), b.id.clone());
    r.commit(wire("t1", vec![(a, tar_a), (b, tar_b)], Vec::new()).as_slice())
        .unwrap();
    let attempt = r.store.attempt_dir(&ida, "v1", 1);
    std::fs::write(attempt.join("tlc.log"), "Model checking completed.\n").unwrap();
    let mut out = Vec::new();
    let req = FetchRequest {
        id: ida.clone(),
        run: None,
    };
    r.fetch(&req, &mut out).unwrap();
    let mut lines = out.splitn(2, |b| *b == b'\n');
    let reply: FetchReply = serde_json::from_slice(lines.next().unwrap()).unwrap();
    let names: Vec<(u32, &str)> = reply
        .files
        .iter()
        .map(|f| (f.attempt, f.name.as_str()))
        .collect();
    assert_eq!(names, [(1, "result.json"), (1, "tlc.log")]);
    let total: u64 = reply.files.iter().map(|f| f.len).sum();
    let rest = lines.next().unwrap();
    assert_eq!(rest.len() as u64, total);
    assert!(rest.ends_with(b"Model checking completed.\n"));
    let mut queued = Vec::new();
    let req = FetchRequest {
        id: idb,
        run: Some("v1".to_owned()),
    };
    r.fetch(&req, &mut queued).unwrap();
    let reply: FetchReply = serde_json::from_slice(queued.strip_suffix(b"\n").unwrap()).unwrap();
    assert_eq!(
        (reply.job.state, reply.files),
        (crate::proto::VmState::Queued, Vec::new())
    );
}

#[test]
fn a_poll_reports_a_running_job_s_time_and_counters_and_asked_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _) = runner(dir.path());
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    r.commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    let attempt = r.store.attempt_dir(&id, "v1", 1);
    let so_far = AttemptResult {
        duration_secs: 420,
        ..AttemptResult::default()
    };
    crate::files::write_json(&attempt.join("result.json"), &so_far).unwrap();
    // The counters are the curve's last line, which carries generated states
    // across a recovery as TLC's log does not.
    std::fs::write(
        attempt.join("curve.tsv"),
        "# tlcq curve\n400\t9\t3000\t1000\t200\n",
    )
    .unwrap();
    let mut req = preflight(&[&id], &[]);
    req.runs = vec![
        crate::proto::RunAsk {
            id: id.clone(),
            run: "v1".to_owned(),
        },
        crate::proto::RunAsk {
            id: id.clone(),
            run: "v9".to_owned(),
        },
    ];
    let reply = r.status(&req).unwrap();
    let job = &reply.jobs[&id];
    assert_eq!(job.elapsed_secs, Some(420));
    let counters = crate::proto::Counters {
        generated: 3_000,
        distinct: 1_000,
        queue: 200,
    };
    assert_eq!(job.counters, Some(counters));
    assert_eq!(reply.runs[0].1.counters, Some(counters));
    assert_eq!(reply.runs[1].1.state, crate::proto::VmState::Unknown);
}

#[test]
fn a_poll_reports_lane_decisions_of_running_runs_and_paused_ones() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    r.commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    let decision = crate::outcome::LaneDecision {
        to: crate::outcome::Lane::Slow,
        after_secs: 900,
        probe: None,
    };
    let attempt = r.store.attempt_dir(&id, "v1", 1);
    let so_far = AttemptResult {
        lanes: vec![decision.clone()],
        ..AttemptResult::default()
    };
    crate::files::write_json(&attempt.join("result.json"), &so_far).unwrap();
    let reply = r.status(&preflight(&[&id], &[])).unwrap();
    assert_eq!(reply.jobs[&id].lanes, std::slice::from_ref(&decision));
    // Paused: the ended attempt's decisions, running to the laptop, held paused.
    std::fs::write(attempt.join("started"), "").unwrap();
    std::fs::write(attempt.join("paused"), "").unwrap();
    let at = r.store.checkpoint_dir(&id, "v1");
    std::fs::create_dir_all(at.join("tlc")).unwrap();
    std::fs::write(at.join("tlc/queue.chkpt"), "").unwrap();
    let kept = crate::runner::store::Checkpoint {
        workers: 16,
        fp: None,
        generated: None,
        attempt: Some(1),
    };
    crate::files::write_json(&at.join("checkpoint.json"), &kept).unwrap();
    fake.gone(&container_name(&id, "v1", 1));
    let reply = r.status(&preflight(&[&id], &[])).unwrap();
    let job = &reply.jobs[&id];
    assert_eq!(
        (
            job.lanes.clone(),
            job.counters,
            job.state.clone(),
            job.held.clone()
        ),
        (
            vec![decision],
            None,
            crate::proto::VmState::Running,
            Some(Held::Paused {
                at: None,
                died: None,
                checkpoint_mb: Some(0)
            })
        )
    );
}

#[test]
fn a_reply_carries_each_run_s_last_attempt_curve() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _fake) = runner(dir.path());
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    r.commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    let attempt = r.store.attempt_dir(&id, "v1", 1);
    std::fs::write(attempt.join("curve.tsv"), "# tlcq curve\n60\t3\t9\t5\t2\n").unwrap();
    let polled = r.status(&preflight(&[&id], &[])).unwrap();
    let curve = polled.jobs[&id].curve.clone().unwrap();
    assert_eq!((curve.attempt, curve.text.lines().count()), (1, 2));
    let asked = r.preflight(&preflight(&[&id], &[])).unwrap();
    assert_eq!(asked.jobs[&id].curve, Some(curve));
}

/// D14 leaves this out of the model: a writing verb that reaches the runner
/// before its start-up settling starts a job; then that settling runs.
#[test]
fn start_up_settling_after_a_verb_started_a_job_keeps_its_scratch() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    // An earlier run, ended before the runner restarted.
    let (old, tar) = job("Old", CommitRoute::Queue);
    let old_id = old.id.clone();
    r.commit(wire("t0", vec![(old, tar)], Vec::new()).as_slice())
        .unwrap();
    let ended = r.store.attempt_dir(&old_id, "v1", 1);
    std::fs::write(ended.join("started"), "").unwrap();
    std::fs::create_dir_all(ended.join("scratch")).unwrap();
    fake.gone(&container_name(&old_id, "v1", 1));
    // The verb comes first: it settles the old run and starts the new job.
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    r.commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    let live = r.store.attempt_dir(&id, "v1", 1);
    std::fs::write(live.join("started"), "").unwrap();
    std::fs::create_dir_all(live.join("scratch")).unwrap();
    std::fs::create_dir_all(ended.join("scratch")).unwrap();
    let checkpoints = [&old_id, &id].map(|i| r.store.checkpoint_dir(i, "v1").join("tlc"));
    for c in &checkpoints {
        std::fs::create_dir_all(c).unwrap();
    }
    // Then the start-up round: only the ended attempt's scratch goes, and
    // only the ended run's checkpoint.
    r.tick(true).unwrap();
    assert!(live.join("scratch").exists());
    assert!(!ended.join("scratch").exists());
    assert_eq!(checkpoints.map(|c| c.exists()), [false, true]);
}

#[test]
fn a_settled_attempt_s_result_json_records_the_run_s_end() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    r.commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    let attempt = r.store.attempt_dir(&id, "v1", 1);
    std::fs::write(attempt.join("started"), "").unwrap();
    std::fs::write(attempt.join("timed-out"), "").unwrap();
    std::fs::write(
        attempt.join("tlc.log"),
        "Progress(7) at 2026-10-18 10:00:00: 900 states generated, 300 distinct states found, 40 states left on queue.\n",
    )
    .unwrap();
    fake.gone(&container_name(&id, "v1", 1));
    r.status(&preflight(&[&id], &[])).unwrap();
    let written: AttemptResult = crate::files::read_json(&attempt.join("result.json"))
        .unwrap()
        .unwrap();
    let end = written.ended.unwrap();
    assert_eq!(
        (end.outcome, end.distinct),
        (crate::outcome::Outcome::Timeout, Some(300))
    );
}

#[test]
fn a_commit_s_reply_gives_each_job_s_slot_or_queue_place() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _fake) = runner(dir.path());
    let jobs = ["A", "B", "C"].map(|s| job(s, CommitRoute::Queue));
    let ids = jobs.clone().map(|(j, _)| j.id);
    let reply = r
        .commit(wire("t1", jobs.to_vec(), Vec::new()).as_slice())
        .unwrap();
    let CommitReply::Applied { views, .. } = reply else {
        panic!("not applied: {reply:?}");
    };
    assert_eq!(views[&ids[0]].place.as_deref(), Some("vm E"));
    let places: Vec<Option<usize>> = ids[1..].iter().map(|id| views[id].position).collect();
    assert_eq!(places, [Some(1), Some(2)]);
    // The queue's places count queued runs only, and a stopping run names its canceller.
    let state = |r: &Runner<Fake>| {
        let lock = r.store.lock().unwrap();
        r.store.load(&lock).unwrap()
    };
    assert_eq!(state(&r).run_job(&ids[2], "v1").position, Some(2));
    r.cancel(&crate::proto::CancelRequest {
        id: ids[0].clone(),
        run: None,
        by: "w1".to_owned(),
    })
    .unwrap();
    assert_eq!(
        state(&r).vm_job(&ids[0]).cancelled,
        Some(("w1".to_owned(), None))
    );
}

#[test]
fn a_poll_says_what_a_queued_job_waits_for_and_when_a_running_one_started() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _fake) = runner(dir.path());
    let jobs = ["A", "B"].map(|s| job(s, CommitRoute::Queue));
    let ids = jobs.clone().map(|(j, _)| j.id);
    r.commit(wire("t1", jobs.to_vec(), Vec::new()).as_slice())
        .unwrap();
    let so_far = AttemptResult {
        started_at: Some(1_000),
        ..AttemptResult::default()
    };
    crate::files::write_json(
        &r.store.attempt_dir(&ids[0], "v1", 1).join("result.json"),
        &so_far,
    )
    .unwrap();
    let ask = preflight(&[&ids[0], &ids[1]], &[]);
    let reply = r.status(&ask).unwrap();
    assert_eq!(reply.jobs[&ids[0]].started_at, Some(1_000));
    assert_eq!(reply.jobs[&ids[1]].waits_for.as_deref(), Some("a VM slot"));
    r.disk_floor = u64::MAX;
    let reply = r.preflight(&ask).unwrap();
    assert_eq!(reply.jobs[&ids[1]].waits_for.as_deref(), Some("disk"));
    // The stopping mark holds the queue whatever else is short.
    r.store.set_stopping(true).unwrap();
    let reply = r.preflight(&ask).unwrap();
    assert_eq!(
        reply.jobs[&ids[1]].waits_for.as_deref(),
        Some("the queue to be released")
    );
}

#[test]
fn a_tick_with_the_disk_low_deletes_orphans_and_prunes_images() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let ghost = r.store.attempt_dir("ghost", "v1", 1).join("scratch");
    std::fs::create_dir_all(&ghost).unwrap();
    std::fs::write(ghost.join("fpset"), vec![0; 1 << 20]).unwrap();
    // With room on the disk, nothing goes.
    r.disk_floor = 0;
    r.tick(false).unwrap();
    assert!(ghost.is_dir());
    assert_eq!(fake.0.borrow().pruned, 0);
    // Below twice the floor, the orphan goes and images are pruned; the
    // status reply says so.
    r.disk_floor = u64::MAX / 4;
    r.tick(false).unwrap();
    assert!(!ghost.exists());
    assert_eq!(fake.0.borrow().pruned, 1);
    let disk = r.status(&preflight(&[], &[])).unwrap().disk.unwrap();
    assert!(disk.low(), "{disk:?}");
}

#[test]
fn a_job_waits_for_memory_while_too_little_is_free_and_starts_once_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    fake.0.borrow_mut().full = true;
    let jobs = ["A"].map(|s| job(s, CommitRoute::Queue));
    let id = jobs[0].0.id.clone();
    r.commit(wire("t1", jobs.to_vec(), Vec::new()).as_slice())
        .unwrap();
    assert_eq!(fake.0.borrow().started, Vec::<String>::new());
    let ask = preflight(&[&id], &[]);
    assert_eq!(
        r.status(&ask).unwrap().jobs[&id].waits_for.as_deref(),
        Some("memory")
    );
    // Memory frees: the next round starts it, its first attempt (none was spent waiting).
    fake.0.borrow_mut().full = false;
    r.tick(false).unwrap();
    assert_eq!(fake.0.borrow().started, [container_name(&id, "v1", 1)]);
    assert!(!state(&r).short_of_memory);
}

#[test]
fn a_whole_vm_run_first_in_the_queue_waits_for_the_whole_vm() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    // The fake's free memory (8192 MB) never reaches the request, above a slot's share.
    let (mut a, tar) = job("A", CommitRoute::Queue);
    a.info.memory_mb = Some(12_000);
    let id = a.id.clone();
    r.commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    assert_eq!(fake.0.borrow().started, Vec::<String>::new());
    let ask = preflight(&[&id], &[]);
    assert_eq!(
        r.status(&ask).unwrap().jobs[&id].waits_for.as_deref(),
        Some("the whole VM")
    );
}

#[test]
fn closing_the_p_slot_freezes_a_thawed_job_there_and_starts_nothing_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    r.store.set_p_open(true).unwrap();
    let mut ids = Vec::new();
    for spec in ["A", "B"] {
        let (a, tar) = job(spec, CommitRoute::Queue);
        ids.push(a.id.clone());
        r.commit(wire(spec, vec![(a, tar)], Vec::new()).as_slice())
            .unwrap();
        std::fs::write(
            r.store
                .attempt_dir(&ids[ids.len() - 1], "v1", 1)
                .join("started"),
            "",
        )
        .unwrap();
    }
    let name = |i: usize| container_name(&ids[i], "v1", 1);
    // A job running in P runs on when it closes.
    r.set_p_open(false).unwrap();
    assert!(!r.store.p_open() && fake.0.borrow().paused.is_empty());
    r.set_p_open(true).unwrap();
    // B frozen (and not held) thaws back into P, the only slot free, on
    // spare capacity.
    assert!(r.freeze(&ids[1], "v1").unwrap());
    unhold(&r, &ids[1]);
    r.tick(false).unwrap();
    let b = state(&r)
        .latest(&ids[1])
        .unwrap()
        .live_attempt()
        .unwrap()
        .clone();
    assert_eq!((b.freeze, b.slot), (Freeze::Thawed, Slot::P));
    // Closing P freezes it; when A ends, it thaws into E, never P.
    r.set_p_open(false).unwrap();
    assert!(fake.0.borrow().paused.contains(&name(1)));
    fake.gone(&name(0));
    r.tick(false).unwrap();
    let b = state(&r)
        .latest(&ids[1])
        .unwrap()
        .live_attempt()
        .unwrap()
        .clone();
    assert_eq!((b.freeze, b.slot), (Freeze::Thawed, Slot::E));
    assert_eq!(fake.0.borrow().slots[&name(1)], Slot::E);
}

#[test]
fn a_thaw_whose_unpause_fails_is_logged_frozen_again_and_stays_frozen() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    r.commit(wire("A", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    std::fs::write(r.store.attempt_dir(&id, "v1", 1).join("started"), "").unwrap();
    fake.0.borrow_mut().no_unpause = true;
    // Frozen (and not held), E free and nothing waiting: the thaw is tried,
    // and fails.
    assert!(r.freeze(&id, "v1").unwrap());
    unhold(&r, &id);
    r.tick(false).unwrap();
    let log = r.store.frozen_log(&id, "v1", 1).unwrap();
    let events: Vec<&str> = log
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(_, e)| e))
        .collect();
    assert_eq!(events, ["freeze", "thaw e", "freeze"]);
    let a = state(&r)
        .latest(&id)
        .unwrap()
        .live_attempt()
        .unwrap()
        .clone();
    assert_eq!(a.freeze, Freeze::Frozen);
}

#[test]
fn the_runner_s_start_makes_each_attempt_s_freezing_what_its_container_is() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    r.store.set_p_open(true).unwrap();
    let mut ids = Vec::new();
    for spec in ["A", "B"] {
        let (a, tar) = job(spec, CommitRoute::Queue);
        ids.push(a.id.clone());
        r.commit(wire(spec, vec![(a, tar)], Vec::new()).as_slice())
            .unwrap();
    }
    let name = |i: usize| container_name(&ids[i], "v1", 1);
    // A's container paused though the state says it runs; B's running though
    // the state says frozen, its last thaw into P logged.
    fake.0.borrow_mut().paused.insert(name(0));
    let lock = r.store.lock().unwrap();
    let mut s = r.store.load(&lock).unwrap();
    s.run_mut(&ids[1], "v1").attempts[0].freeze = Freeze::Frozen;
    r.store.save(&lock, &s).unwrap();
    drop(lock);
    r.store.log_frozen(&ids[1], "v1", 1, "thaw p", 5).unwrap();
    r.tick(true).unwrap();
    let s = state(&r);
    let a = s.latest(&ids[0]).unwrap().live_attempt().unwrap().clone();
    let b = s.latest(&ids[1]).unwrap().live_attempt().unwrap().clone();
    // Frozen, then thawed back into E on spare capacity by the same start.
    let log = r.store.frozen_log(&ids[0], "v1", 1).unwrap();
    let events: Vec<&str> = log
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(_, e)| e))
        .collect();
    assert_eq!(events, ["freeze", "thaw e"]);
    assert_eq!((a.freeze, a.slot), (Freeze::Thawed, Slot::E));
    assert!(a.frozen_at.is_some());
    assert_eq!((b.freeze, b.slot), (Freeze::Thawed, Slot::P));
    // A freeze logged but its container never paused: the log thaws it back,
    // so its clock runs on.
    r.store.log_frozen(&ids[1], "v1", 1, "freeze", 6).unwrap();
    r.tick(true).unwrap();
    let log = r.store.frozen_log(&ids[1], "v1", 1).unwrap();
    assert!(log.ends_with(" thaw p\n"), "{log}");
    assert_eq!(
        state(&r)
            .latest(&ids[1])
            .unwrap()
            .live_attempt()
            .unwrap()
            .freeze,
        Freeze::Thawed
    );
}

#[test]
fn a_cancelled_frozen_job_is_not_unfrozen() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    r.commit(wire("A", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    std::fs::write(r.store.attempt_dir(&id, "v1", 1).join("started"), "").unwrap();
    fake.0.borrow_mut().full = true;
    assert!(r.freeze(&id, "v1").unwrap());
    r.cancel(&CancelRequest {
        id: id.clone(),
        run: Some("v1".to_owned()),
        by: "agent".to_owned(),
    })
    .unwrap();
    assert!(!r.unfreeze(&id, "v1").unwrap());
}

#[test]
fn the_last_thaw_logged_names_its_slot() {
    assert_eq!(last_thaw("1 thaw e\n2 freeze\n"), Some(Slot::E));
    assert_eq!(last_thaw("1 thaw e\n2 thaw p\n3 unpause\n"), Some(Slot::P));
    assert_eq!(last_thaw("1 freeze\n2 thaw\n"), None);
}

#[test]
fn keep_running_withdraws_a_pause_until_its_supervisor_marks_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    r.commit(wire("A", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    let dir1 = r.store.attempt_dir(&id, "v1", 1);
    std::fs::write(dir1.join("started"), "").unwrap();
    // Not pausing: nothing to withdraw.
    assert!(!r.keep(&id, "v1").unwrap());
    assert!(r.pause(&id, "v1").unwrap());
    assert!(r.keep(&id, "v1").unwrap());
    assert!(!dir1.join("pause").exists());
    let a = state(&r)
        .latest(&id)
        .unwrap()
        .live_attempt()
        .unwrap()
        .clone();
    assert_eq!((a.freeze, a.pausing), (Freeze::Running, false));
    // A thawed one paused and kept stays thawed: admission may freeze it again.
    let lock = r.store.lock().unwrap();
    let mut s = r.store.load(&lock).unwrap();
    s.run_mut(&id, "v1").attempts[0].freeze = Freeze::Thawed;
    r.store.save(&lock, &s).unwrap();
    drop(lock);
    assert!(r.pause(&id, "v1").unwrap());
    assert!(r.keep(&id, "v1").unwrap());
    let a = state(&r)
        .latest(&id)
        .unwrap()
        .live_attempt()
        .unwrap()
        .clone();
    assert_eq!((a.freeze, a.pausing), (Freeze::Thawed, false));
    // Once marked paused, its stop is under way: refused.
    assert!(r.pause(&id, "v1").unwrap());
    std::fs::write(dir1.join("paused"), "").unwrap();
    assert!(!r.keep(&id, "v1").unwrap());
    assert!(dir1.join("pause").exists());
    // One unpaused from frozen for its pause holds no slot: refused too.
    std::fs::remove_file(dir1.join("paused")).unwrap();
    let lock = r.store.lock().unwrap();
    let mut s = r.store.load(&lock).unwrap();
    s.run_mut(&id, "v1").attempts[0].freeze = Freeze::Unpaused;
    r.store.save(&lock, &s).unwrap();
    drop(lock);
    assert!(!r.keep(&id, "v1").unwrap());
    assert!(fake.0.borrow().paused.is_empty());
}

#[test]
fn an_agent_s_resume_asks_for_memory_and_says_why_it_did_not_resume() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let id = paused_job(&mut r, &fake, "A", &AttemptResult::default());
    let ask = |id: &str, memory_mb| crate::proto::ResumeRequest {
        timeout_secs: None,
        id: id.to_owned(),
        run: None,
        memory_mb,
    };
    // Past the whole VM, or of no live run: refused, nothing changed.
    let refused = |reply: ResumeReply| reply.refused.unwrap();
    assert!(refused(r.resume_run(&ask(&id, Some(16_385))).unwrap())
        .starts_with("it asks for 16385 MB, more than the 16384 MB"));
    assert_eq!(
        refused(r.resume_run(&ask("nope", None)).unwrap()),
        "it has no live run on the VM"
    );
    assert!(state(&r).latest(&id).unwrap().paused());
    // Within what is free: it starts in E with its request.
    let reply = r.resume_run(&ask(&id, Some(8_192))).unwrap();
    assert_eq!(reply.refused, None);
    assert_eq!(reply.job.run.as_deref(), Some("v1"));
    let s = state(&r);
    let run = s.latest(&id).unwrap();
    assert_eq!(run.info.memory_mb, Some(8_192));
    assert_eq!(
        run.attempts
            .last()
            .map(|a| (a.number, a.resume, a.memory_mb)),
        Some((2, true, Some(8_192)))
    );
    // Running again: not paused.
    assert_eq!(
        refused(r.resume_run(&ask(&id, None)).unwrap()),
        "it is not paused on the VM"
    );
}

#[test]
fn a_long_run_gone_with_its_supervisor_keeps_its_checkpoint_and_pauses() {
    // A reboot: the container stopped, the supervisor with it, no end written.
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    r.store.set_p_open(true).unwrap();
    let mut ids = Vec::new();
    for (spec, secs) in [("A", 3600), ("B", 3599)] {
        let (a, tar) = job(spec, CommitRoute::Queue);
        let id = a.id.clone();
        r.commit(wire(spec, vec![(a, tar)], Vec::new()).as_slice())
            .unwrap();
        let attempt = r.store.attempt_dir(&id, "v1", 1);
        std::fs::write(attempt.join("started"), "").unwrap();
        let so_far = AttemptResult {
            duration_secs: secs,
            ..AttemptResult::default()
        };
        crate::files::write_json(&attempt.join("result.json"), &so_far).unwrap();
        std::fs::create_dir_all(attempt.join("scratch/A")).unwrap();
        std::fs::write(attempt.join("scratch/A/queue.chkpt"), "").unwrap();
        std::fs::write(attempt.join("tlc.log"), "").unwrap();
        fake.gone(&container_name(&id, "v1", 1));
        ids.push(id);
    }
    r.tick(false).unwrap();
    let s = state(&r);
    let long = s.latest(&ids[0]).unwrap();
    assert!(long.paused());
    assert_eq!(
        long.attempts[0].died.as_deref(),
        Some("stopped: a shutdown or out of memory")
    );
    let kept = r.store.checkpoint(&ids[0], "v1").unwrap().unwrap();
    assert_eq!((kept.workers, kept.attempt), (16, Some(1)));
    assert!(!r.store.checkpoint_dir(&ids[0], "v1").join("tlx").exists());
    // Under the hour: it ends, its checkpoint gone with its scratch.
    assert!(matches!(
        s.latest(&ids[1]).unwrap().phase,
        Phase::Ended { .. }
    ));
    assert_eq!(r.store.checkpoint(&ids[1], "v1").unwrap(), None);
}

#[test]
fn a_long_run_its_older_supervisor_stopped_at_its_cap_is_paused_and_resumes_with_a_longer_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    // Paused, then resumed: attempt 2 recovers attempt 1's checkpoint.
    let id = paused_job(&mut r, &fake, "A", &AttemptResult::default());
    assert!(r.resume(&id, "v1").unwrap());
    // An older supervisor stopped it at its cap and kept nothing.
    let attempt = r.store.attempt_dir(&id, "v1", 2);
    std::fs::write(attempt.join("started"), "").unwrap();
    std::fs::write(attempt.join("timed-out"), "").unwrap();
    let capped = AttemptResult {
        duration_secs: 43_200,
        ended_at: Some(1),
        ..AttemptResult::default()
    };
    crate::files::write_json(&attempt.join("result.json"), &capped).unwrap();
    fake.gone(&container_name(&id, "v1", 2));
    r.tick(false).unwrap();
    let s = state(&r);
    let run = s.latest(&id).unwrap();
    assert!(run.paused(), "{:?}", run.phase);
    assert_eq!(
        run.attempts.last().and_then(|a| a.died.as_deref()),
        Some("stopped at its time cap: resume it with a longer --timeout")
    );
    assert_eq!(
        r.store.checkpoint(&id, "v1").unwrap().map(|c| c.attempt),
        Some(Some(2))
    );
    // A cap at or below the time run is refused; above, it resumes with it.
    let ask = |timeout_secs| crate::proto::ResumeRequest {
        id: id.clone(),
        run: None,
        memory_mb: None,
        timeout_secs,
    };
    let reply = r.resume_run(&ask(Some(43_200))).unwrap();
    assert_eq!(
        reply.refused.as_deref(),
        Some("it has run 43200 s already: give it a --timeout above that")
    );
    assert_eq!(r.resume_run(&ask(Some(86_400))).unwrap().refused, None);
    assert_eq!(
        state(&r).latest(&id).unwrap().info.timeout_secs,
        Some(86_400)
    );
}

/// Let run `v1` of `id`, frozen by a human, be thawed on spare capacity, as
/// one admission froze again for new work is.
pub(crate) fn unhold(r: &Runner<Fake>, id: &str) {
    let lock = r.store.lock().unwrap();
    let mut s = r.store.load(&lock).unwrap();
    s.run_mut(id, "v1").attempts.last_mut().unwrap().held = false;
    r.store.save(&lock, &s).unwrap();
}

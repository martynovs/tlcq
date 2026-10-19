//! Tests of `status` against a runner in the same process.

use super::*;
use crate::config::Expect;
use crate::store::Follow;
use crate::submit::tests::{finish, job, label, submit_in, world, World};

fn status_of(w: &mut World, ids: &[&str]) -> Status {
    let ids: Vec<String> = ids.iter().map(|s| (*s).to_owned()).collect();
    status(
        &w.store,
        &mut w.vm,
        &crate::engine::test_versions("J"),
        &label(),
        &ids,
    )
    .unwrap()
}

fn views(s: &Status) -> Vec<String> {
    s.lines
        .iter()
        .map(|l| match &l.view {
            View::Vm(job) => format!("vm {:?}", job.state),
            View::Ended(r) => format!("ended {:?}", r.outcome),
            View::Laptop(_) => "Laptop".to_owned(),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn a_job_is_followed_from_running_to_its_end() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    let s = status_of(&mut w, &[]);
    assert_eq!(
        (views(&s), s.exit, s.unreachable),
        (vec!["vm Running".to_owned()], 0, false)
    );
    assert_eq!(s.lines[0].run.as_deref(), Some("v1"));
    finish(&w.vm.runner, &w.vm.fake, &x.id, Outcome::Unexpected);
    let s = status_of(&mut w, &[]);
    assert_eq!(
        (views(&s), s.exit),
        (vec!["ended Unexpected".to_owned()], 1)
    );
    assert_eq!(w.store.record(&x.id).unwrap().unwrap().state, State::Ended);
    assert!(w.store.cached(&x.id).unwrap().is_some());
}

#[test]
fn a_superseded_version_drops_out_and_the_new_one_is_reported() {
    let mut w = world();
    let old = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&old), 0, false, "t1");
    let new = job("A.cfg", "INIT Init2\n", None);
    submit_in(&mut w, std::slice::from_ref(&new), 0, true, "t2");
    let s = status_of(&mut w, &[]);
    let ids: Vec<&str> = s.lines.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, [new.id.as_str()]);
    let named = status_of(&mut w, &[&old.id]);
    assert_eq!(
        (views(&named), named.exit),
        (vec!["vm Stopping".to_owned()], 3)
    );
}

#[test]
fn an_older_version_still_queued_with_its_entry_is_reported_beside_the_latest() {
    let mut w = world();
    let first = job("A.cfg", "INIT Init\n", None);
    let second = job("B.cfg", "INIT Other\n", Some(Expect::Slow));
    submit_in(&mut w, &[first, second.clone()], 0, false, "t1");
    let mut again = second.clone();
    again.names[0].name = "C.cfg".to_owned();
    again.names[0].keys.cfg_name = "C.cfg".to_owned();
    submit_in(&mut w, std::slice::from_ref(&again), 0, false, "t2");
    let s = status_of(&mut w, &[]);
    assert_eq!(s.lines.len(), 3);
    assert!(views(&s).contains(&"vm Queued".to_owned()));
}

#[test]
fn ids_are_reported_by_their_follow_else_their_latest_run() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    let s = status_of(&mut w, &[&x.id, "nope"]);
    assert_eq!(views(&s), ["vm Running", "Unknown"]);
    assert_eq!(s.exit, 3);
    let other = Label {
        worktree: "/elsewhere".into(),
        ..label()
    };
    let s = status(
        &w.store,
        &mut w.vm,
        &crate::engine::test_versions("J"),
        &other,
        std::slice::from_ref(&x.id),
    )
    .unwrap();
    assert_eq!(
        (s.lines[0].key.clone(), views(&s)),
        (None, vec!["vm Running".to_owned()])
    );
}

#[test]
fn an_unreachable_vm_reports_the_last_known_state() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    w.vm.down = true;
    let s = status_of(&mut w, &[]);
    assert_eq!(
        (views(&s), s.unreachable, s.exit),
        (vec!["LastKnown(Vm)".to_owned()], true, 0)
    );
    let s = status_of(&mut w, &[&x.id, "nope"]);
    assert_eq!(views(&s), ["LastKnown(Vm)", "Unknown"]);
}

#[test]
fn a_lost_reply_is_reported_sent_until_a_poll_resolves_it() {
    let mut w = world();
    w.vm.lose_reply = true;
    w.vm.down = false;
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    w.vm.down = true;
    assert_eq!(views(&status_of(&mut w, &[])), ["Sent"]);
    w.vm.down = false;
    let s = status_of(&mut w, &[]);
    assert_eq!(views(&s), ["vm Running"]);
    assert_eq!(s.lines[0].run.as_deref(), Some("v1"));
    assert_eq!(w.store.record(&x.id).unwrap().unwrap().state, State::Vm);
}

#[test]
fn every_view_has_its_exit_code() {
    let ended = |o| View::Ended(crate::runner::state::State::outcome_only(o));
    assert_eq!(exit_of(&ended(Outcome::Ok)), 0);
    assert_eq!(exit_of(&ended(Outcome::Timeout)), 3);
    assert_eq!(exit_of(&View::NeverSent), 3);
    assert_eq!(exit_of(&View::Laptop(Progress::default())), 0);
    let mut r = JobRecord::new("a", State::Laptop);
    assert_eq!(record_view(&r, false), View::Laptop(Progress::default()));
    r.state = State::Sent;
    assert_eq!(record_view(&r, false), View::Sent);
    r.state = State::Vm;
    assert_eq!(record_view(&r, false), View::Unknown);
}

#[test]
fn another_agent_s_id_reads_the_laptop_s_record_while_the_vm_is_unreachable() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    w.vm.down = true;
    let other = Label {
        worktree: "/elsewhere".into(),
        ..label()
    };
    let s = status(
        &w.store,
        &mut w.vm,
        &crate::engine::test_versions("J"),
        &other,
        std::slice::from_ref(&x.id),
    )
    .unwrap();
    assert_eq!(views(&s), ["LastKnown(Vm)"]);
}

#[test]
fn each_view_of_a_follow() {
    let ok = crate::runner::state::State::outcome_only(Outcome::Ok);
    let follow = |what| Follow {
        number: 1,
        key: crate::runner::state::tests::entry("A.cfg").key,
        label: label(),
        id: "a".to_owned(),
        what,
    };
    assert_eq!(
        follow_view(&follow(Followed::Result(ok.clone())), None, None),
        View::Ended(ok.clone())
    );
    assert_eq!(
        follow_view(&follow(Followed::NeverSent), None, None),
        View::NeverSent
    );
    let run = follow(Followed::Run(RunRef::Run("v1".to_owned())));
    assert_eq!(follow_view(&run, None, None), View::Unknown);
    let mut ended = JobRecord::new("a", State::Ended);
    ended.run = Some("v1".to_owned());
    ended.result = Some(ok.clone());
    assert_eq!(
        follow_view(&run, Some(&ended), None),
        View::Ended(ok.clone())
    );
    let unknown = VmJob {
        state: VmState::Unknown,
        run: None,
        entries: Vec::new(),
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
    };
    assert_eq!(vm_view(&unknown), View::Unknown);
    let key = &run.key;
    let line = |view| Line {
        key: None,
        id: "a".to_owned(),
        run: None,
        view,
        eta: None,
        engine: None,
    };
    // A record holding the key's entry: on the laptop, and as the VM's mirror.
    let entry = crate::entry::Entry {
        key: key.clone(),
        label: label(),
    };
    let mut held = JobRecord::new("a", State::Laptop);
    held.entries = vec![entry.clone()];
    held.mirror = vec![entry.clone()];
    for (view, holds) in [
        (View::Sent, true),
        (View::Laptop(Progress::default()), true),
        (View::LastKnown(State::Vm), true),
        (View::Ended(ok), false),
        (View::NeverSent, false),
        (View::Unknown, false),
        (View::Vm(unknown), false),
    ] {
        assert_eq!(still_holds(&line(view), key, Some(&held)), holds);
    }
    // The entry gone (superseded for this key, kept for another worktree):
    // no longer reported; a pending entry still holds it.
    let mut gone = JobRecord::new("a", State::Laptop);
    for view in [
        View::Laptop(Progress::default()),
        View::LastKnown(State::Vm),
    ] {
        assert!(!still_holds(&line(view.clone()), key, Some(&gone)));
        assert!(!still_holds(&line(view), key, None));
    }
    gone.pending.push(crate::store::Pending {
        entry,
        run: RunRef::Run("v1".to_owned()),
    });
    assert!(still_holds(
        &line(View::LastKnown(State::Vm)),
        key,
        Some(&gone)
    ));
}

/// A VM whose every call connects and then fails.
struct Broken;

impl Remote for Broken {
    fn preflight(
        &mut self,
        _: &crate::proto::PreflightRequest,
    ) -> Result<PreflightReply, CallError> {
        Err(CallError::Failed("cut".to_owned()))
    }

    fn status(&mut self, _: &crate::proto::PreflightRequest) -> Result<PreflightReply, CallError> {
        Err(CallError::Failed("cut".to_owned()))
    }

    fn commit(&mut self, _: &[u8]) -> Result<crate::commit::CommitReply, CallError> {
        Err(CallError::Failed("cut".to_owned()))
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

#[test]
fn a_laptop_run_is_reported_and_a_poll_that_keeps_failing_reads_the_last_known_states() {
    let mut w = world();
    w.vm.down = true;
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 1, false, "t1");
    assert_eq!(views(&status_of(&mut w, &[])), ["Laptop"]);
    let s = status(
        &w.store,
        &mut Broken,
        &crate::engine::test_versions("J"),
        &label(),
        &[],
    )
    .unwrap();
    assert_eq!(
        (views(&s), s.vm_error.as_deref(), s.exit),
        (vec!["Laptop".to_owned()], Some("cut"), 0)
    );
    Broken
        .preflight(&crate::proto::PreflightRequest::default())
        .unwrap_err();
    Broken.commit(b"").unwrap_err();
}

#[test]
fn an_older_run_holds_its_place_while_it_runs_with_its_key_s_entry() {
    let entry = crate::runner::state::tests::entry("A.cfg");
    let job = |entries| VmJob {
        state: VmState::Running,
        run: Some("v1".to_owned()),
        entries,
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
    };
    let line = |view| Line {
        key: None,
        id: "a".to_owned(),
        run: None,
        view,
        eta: None,
        engine: None,
    };
    assert!(still_holds(
        &line(View::Vm(job(vec![entry.clone()]))),
        &entry.key,
        None
    ));
    assert!(!still_holds(
        &line(View::Vm(job(Vec::new()))),
        &entry.key,
        None
    ));
}

/// A VM whose polls fail `fails` times, then answer as `inner` does.
struct Flaky<'a> {
    fails: usize,
    calls: usize,
    inner: &'a mut crate::submit::tests::Local,
}

impl Remote for Flaky<'_> {
    fn preflight(
        &mut self,
        req: &crate::proto::PreflightRequest,
    ) -> Result<PreflightReply, CallError> {
        self.inner.preflight(req)
    }

    fn status(
        &mut self,
        req: &crate::proto::PreflightRequest,
    ) -> Result<PreflightReply, CallError> {
        self.calls += 1;
        if self.calls <= self.fails {
            return Err(CallError::Failed("dropped".to_owned()));
        }
        self.inner.status(req)
    }

    fn commit(&mut self, wire: &[u8]) -> Result<crate::commit::CommitReply, CallError> {
        self.inner.commit(wire)
    }

    fn ls(&mut self) -> Result<std::collections::BTreeMap<String, VmJob>, CallError> {
        self.inner.ls()
    }

    fn cancel(&mut self, req: &crate::proto::CancelRequest) -> Result<VmJob, CallError> {
        self.inner.cancel(req)
    }

    fn resume(
        &mut self,
        req: &crate::proto::ResumeRequest,
    ) -> Result<crate::proto::ResumeReply, CallError> {
        self.inner.resume(req)
    }

    fn fetch(&mut self, req: &crate::proto::FetchRequest) -> Result<Vec<u8>, CallError> {
        self.inner.fetch(req)
    }
}

#[test]
fn a_poll_that_fails_is_retried() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    let mut flaky = Flaky {
        fails: 2,
        calls: 0,
        inner: &mut w.vm,
    };
    let s = status(
        &w.store,
        &mut flaky,
        &crate::engine::test_versions("J"),
        &label(),
        &[],
    )
    .unwrap();
    assert_eq!(
        (views(&s), s.vm_error, flaky.calls),
        (vec!["vm Running".to_owned()], None, 3)
    );
    flaky
        .preflight(&crate::proto::PreflightRequest::default())
        .unwrap();
    flaky.ls().unwrap();
    flaky
        .fetch(&crate::proto::FetchRequest {
            id: x.id.clone(),
            run: None,
        })
        .unwrap();
    flaky
        .cancel(&crate::proto::CancelRequest {
            id: x.id.clone(),
            run: None,
            by: "t".to_owned(),
        })
        .unwrap();
    flaky.commit(b"not json\n").unwrap_err();
}

/// A VM whose first poll races a command that rewrites a record meanwhile.
struct Racing<'a> {
    polls: usize,
    store: &'a Store,
    id: String,
    inner: &'a mut crate::submit::tests::Local,
}

impl Remote for Racing<'_> {
    fn preflight(
        &mut self,
        req: &crate::proto::PreflightRequest,
    ) -> Result<PreflightReply, CallError> {
        self.inner.preflight(req)
    }

    fn status(
        &mut self,
        req: &crate::proto::PreflightRequest,
    ) -> Result<PreflightReply, CallError> {
        self.polls += 1;
        if self.polls == 1 {
            let failed = |e: std::io::Error| CallError::Failed(e.to_string());
            let lock = self.store.placement_lock().map_err(failed)?;
            let record = self.store.record(&self.id).map_err(failed)?;
            if let Some(mut r) = record {
                self.store.write_record(&lock, &mut r).map_err(failed)?;
            }
        }
        self.inner.status(req)
    }

    fn commit(&mut self, wire: &[u8]) -> Result<crate::commit::CommitReply, CallError> {
        self.inner.commit(wire)
    }

    fn ls(&mut self) -> Result<std::collections::BTreeMap<String, VmJob>, CallError> {
        self.inner.ls()
    }

    fn cancel(&mut self, req: &crate::proto::CancelRequest) -> Result<VmJob, CallError> {
        self.inner.cancel(req)
    }

    fn resume(
        &mut self,
        req: &crate::proto::ResumeRequest,
    ) -> Result<crate::proto::ResumeReply, CallError> {
        self.inner.resume(req)
    }

    fn fetch(&mut self, req: &crate::proto::FetchRequest) -> Result<Vec<u8>, CallError> {
        self.inner.fetch(req)
    }
}

#[test]
fn a_record_changed_during_the_poll_is_polled_again() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    let mut racing = Racing {
        polls: 0,
        store: &w.store,
        id: x.id.clone(),
        inner: &mut w.vm,
    };
    let s = status(
        &w.store,
        &mut racing,
        &crate::engine::test_versions("J"),
        &label(),
        &[],
    )
    .unwrap();
    assert_eq!(
        (views(&s), racing.polls),
        (vec!["vm Running".to_owned()], 2)
    );
    racing
        .preflight(&crate::proto::PreflightRequest::default())
        .unwrap();
    racing.ls().unwrap();
    racing
        .fetch(&crate::proto::FetchRequest {
            id: x.id.clone(),
            run: None,
        })
        .unwrap();
    racing
        .cancel(&crate::proto::CancelRequest {
            id: x.id.clone(),
            run: None,
            by: "t".to_owned(),
        })
        .unwrap();
    racing.commit(b"not json\n").unwrap_err();
}

#[test]
fn a_run_the_reply_did_not_cover_reads_as_last_known() {
    let mut r = JobRecord::new("a", State::Vm);
    r.run = Some("v3".to_owned());
    let f = Follow {
        number: 1,
        key: crate::runner::state::tests::entry("A.cfg").key,
        label: label(),
        id: "a".to_owned(),
        what: Followed::Run(RunRef::Run("v3".to_owned())),
    };
    assert_eq!(
        follow_view(&f, Some(&r), Some(&PreflightReply::default())),
        View::LastKnown(State::Vm)
    );
}

/// A laptop job whose agent follows run `l1`, its record moved on to `l2`,
/// and `l1`'s result in its run directory.
pub(crate) fn moved_on_from_l1(w: &mut World) -> String {
    w.vm.down = true;
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(w, std::slice::from_ref(&x), 1, false, "t1");
    let lock = w.store.placement_lock().unwrap();
    let mut r = w.store.record(&x.id).unwrap().unwrap();
    r.run = Some("l2".to_owned());
    w.store.write_record(&lock, &mut r).unwrap();
    let ok = crate::laptop_runs::laptop_result(Outcome::Ok, 30);
    crate::files::write_json(&w.store.run_dir(&x.id, "l1").join("result.json"), &ok).unwrap();
    x.id
}

#[test]
fn an_earlier_laptop_run_reports_the_result_in_its_directory() {
    let mut w = world();
    let id = moved_on_from_l1(&mut w);
    let s = status_of(&mut w, &[]);
    assert_eq!(views(&s), ["ended Ok"]);
    assert_eq!(s.lines[0].run.as_deref(), Some("l1"));
    let named = status_of(&mut w, &["nope"]);
    assert_eq!((views(&named), named.exit), (vec!["Unknown".to_owned()], 3));
    assert_eq!(w.store.record(&id).unwrap().unwrap().state, State::Laptop);
}

/// D14 leaves this out of the model: a commit held after its client gave up,
/// while a poll looks its token up, then released.
#[test]
fn a_commit_released_after_a_poll_looked_its_token_up_applies_nothing() {
    let mut w = world();
    w.vm.hold = true;
    let x = job("A.cfg", "INIT Init\n", None);
    let report = submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    assert_eq!(report.exit, 3);
    w.vm.hold = false;
    // The poll looks the token up: unknown to the VM, it is tombstoned.
    let polled = status_of(&mut w, &[]);
    assert_eq!(views(&polled), ["NeverSent"]);
    // The held commit arrives: it applies nothing.
    let held = w.vm.held.take().unwrap();
    let reply = w.vm.runner.commit(held.as_slice()).unwrap();
    assert_eq!(reply, crate::commit::CommitReply::NotApplied);
    let lock = w.vm.runner.store.lock().unwrap();
    assert!(w.vm.runner.store.load(&lock).unwrap().runs.is_empty());
    drop(lock);
    let again = status_of(&mut w, &[]);
    assert_eq!(
        (views(&again), again.exit),
        (vec!["NeverSent".to_owned()], 3)
    );
    let r = w.store.record(&x.id).unwrap().unwrap();
    assert_eq!(r.result.unwrap().outcome, Outcome::NeverSent);
}

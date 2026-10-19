//! Tests of `ls`, `cancel` and `fetch` against a runner in the same process.

use super::*;
use crate::config::Expect;
use crate::outcome::Outcome;
use crate::submit::tests::{job, label, submit_in, world, World};

fn views(s: &Status) -> Vec<String> {
    s.lines.iter().map(|l| view_name(&l.view)).collect()
}

fn view_name(v: &View) -> String {
    match v {
        View::Vm(job) => format!("vm {:?}", job.state),
        View::Ended(r) => format!("ended {:?}", r.outcome),
        View::Laptop(p) => format!("laptop {:?}", p.phase),
        other => format!("{other:?}"),
    }
}

/// Two jobs: `x` running, `y` queued behind it.
fn two(w: &mut World) -> (String, String) {
    let x = job("A.cfg", "INIT Init\n", None);
    let y = job("B.cfg", "INIT Other\n", Some(Expect::Slow));
    submit_in(w, &[x.clone(), y.clone()], 0, false, "t1");
    (x.id, y.id)
}

fn done(d: &Done) -> (String, usize) {
    match d {
        Done::Done { line, files } => (view_name(&line.view), files.len()),
        Done::Refused(why) => (format!("refused: {why}"), 0),
    }
}

#[test]
fn ls_lists_the_vm_s_jobs_and_the_laptop_s_records() {
    let mut w = world();
    let (x, y) = two(&mut w);
    let s = ls(&w.store, &mut w.vm).unwrap();
    let mut got: Vec<(String, String)> = s
        .lines
        .iter()
        .map(|l| (l.id.clone(), view_name(&l.view)))
        .collect();
    got.sort();
    let mut want = vec![
        (x.clone(), "vm Running".to_owned()),
        (y.clone(), "vm Queued".to_owned()),
    ];
    want.sort();
    assert_eq!(got, want);
    assert!(s.lines.iter().all(|l| l.key.is_some()));
    w.vm.down = true;
    let s = ls(&w.store, &mut w.vm).unwrap();
    assert_eq!(
        (views(&s), s.unreachable),
        (vec!["LastKnown(Vm)".to_owned(); 2], true)
    );
}

#[test]
fn cancel_stops_a_running_run_and_ends_a_queued_one() {
    let mut w = world();
    let (x, y) = two(&mut w);
    let running = cancel(&w.store, &mut w.vm, &mut w.sups, &label(), &x).unwrap();
    assert_eq!(
        (done(&running), running.exit()),
        (("vm Stopping".to_owned(), 0), 0)
    );
    let queued = cancel(&w.store, &mut w.vm, &mut w.sups, &label(), &y).unwrap();
    assert!(done(&queued).0.starts_with("ended Cancelled"), "{queued:?}");
    let record = w.store.record(&y).unwrap().unwrap();
    assert_eq!(record.state, State::Ended);
    let again = cancel(&w.store, &mut w.vm, &mut w.sups, &label(), &y).unwrap();
    assert!(done(&again).0.starts_with("ended Cancelled"));
    let unknown = cancel(&w.store, &mut w.vm, &mut w.sups, &label(), "nope").unwrap();
    assert_eq!(
        (done(&unknown).0.as_str(), unknown.exit()),
        ("refused: neither the laptop nor the VM knows it", 2)
    );
    w.vm.down = true;
    let down = cancel(&w.store, &mut w.vm, &mut w.sups, &label(), &x).unwrap();
    assert!(done(&down).0.starts_with("refused: the VM is unreachable"));
}

#[test]
fn a_follow_of_no_run_is_answered_and_touches_nothing() {
    let mut w = world();
    let (x, _) = two(&mut w);
    let follow = |w: &World, what: Followed| {
        let lock = w.store.placement_lock().unwrap();
        let number = w.store.next_follow_number(&lock).unwrap();
        let f = crate::store::Follow {
            number,
            key: crate::runner::state::tests::entry("A.cfg").key,
            label: label(),
            id: x.clone(),
            what,
        };
        w.store.write_follow(&lock, &f).unwrap();
    };
    let out = Path::new("/unused");
    follow(&w, Followed::NeverSent);
    let never = cancel(&w.store, &mut w.vm, &mut w.sups, &label(), &x).unwrap();
    assert_eq!(done(&never), ("NeverSent".to_owned(), 0));
    let cached = crate::runner::state::State::outcome_only(Outcome::Ok);
    follow(&w, Followed::Result(cached));
    let hit = cancel(&w.store, &mut w.vm, &mut w.sups, &label(), &x).unwrap();
    assert_eq!(done(&hit).0, "ended Ok");
    let hit = fetch(&w.store, &mut w.vm, &label(), &x, out).unwrap();
    assert_eq!(done(&hit), ("ended Ok".to_owned(), 0));
    follow(&w, Followed::Run(RunRef::Token("t9".to_owned())));
    let sent = cancel(&w.store, &mut w.vm, &mut w.sups, &label(), &x).unwrap();
    assert!(done(&sent).0.contains("not known yet"), "{sent:?}");
    // The run itself was never touched.
    let lock = w.vm.runner.store.lock().unwrap();
    let state = w.vm.runner.store.load(&lock).unwrap();
    assert!(matches!(
        state.latest(&x).unwrap().phase,
        crate::runner::state::Phase::Running { .. }
    ));
}

#[test]
fn a_job_whose_place_is_uncertain_is_refused_and_a_laptop_job_is_acted_on_here() {
    let mut w = world();
    w.vm.lose_reply = true;
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    let sent = cancel(&w.store, &mut w.vm, &mut w.sups, &label(), &x.id).unwrap();
    assert!(done(&sent).0.contains("uncertain"));
    let mut w = world();
    w.vm.down = true;
    submit_in(&mut w, std::slice::from_ref(&x), 1, false, "t1");
    // Its files are on the laptop, as attempt 0; the VM is not asked.
    let run_dir = w.store.run_dir(&x.id, "l1");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(run_dir.join("tlc.log"), "Starting...").unwrap();
    let out = tempfile::tempdir().unwrap();
    let fetched = fetch(&w.store, &mut w.vm, &label(), &x.id, out.path()).unwrap();
    assert_eq!(done(&fetched), ("laptop Slot { ahead: 0 }".to_owned(), 1));
    assert!(out.path().join(&x.id).join("l1/0/tlc.log").exists());
    let laptop = cancel(&w.store, &mut w.vm, &mut w.sups, &label(), &x.id).unwrap();
    assert!(done(&laptop).0.starts_with("ended Cancelled"), "{laptop:?}");
    // Ended, its result is the laptop's.
    let again = fetch(&w.store, &mut w.vm, &label(), &x.id, out.path()).unwrap();
    assert!(done(&again).0.starts_with("ended Cancelled"), "{again:?}");
    let mut moving = JobRecord::new("m", State::Moving);
    moving.token = Some("t".to_owned());
    assert!(uncertain(Some(&moving), false).is_some_and(|w| w.contains("moving")));
    assert_eq!(uncertain(Some(&moving), true), None);
    let sent = JobRecord::new("s", State::Sent);
    assert!(uncertain(Some(&sent), false).is_some_and(|w| w.contains("not known yet")));
}

#[test]
fn fetch_writes_each_attempt_s_files_under_the_run() {
    let mut w = world();
    let (x, y) = two(&mut w);
    let attempt = w.vm.runner.store.attempt_dir(&x, "v1", 1);
    std::fs::write(attempt.join("tlc.log"), "Model checking completed.\n").unwrap();
    let out = tempfile::tempdir().unwrap();
    let got = fetch(&w.store, &mut w.vm, &label(), &x, out.path()).unwrap();
    assert_eq!(done(&got), ("vm Running".to_owned(), 2));
    let log = out.path().join(&x).join("v1").join("1").join("tlc.log");
    assert_eq!(
        std::fs::read_to_string(log).unwrap(),
        "Model checking completed.\n"
    );
    let queued = fetch(&w.store, &mut w.vm, &label(), &y, out.path()).unwrap();
    assert_eq!(done(&queued), ("vm Queued".to_owned(), 0));
    let unknown = fetch(&w.store, &mut w.vm, &label(), "nope", out.path()).unwrap();
    assert!(done(&unknown).0.starts_with("refused: neither"));
    w.vm.down = true;
    let down = fetch(&w.store, &mut w.vm, &label(), &x, out.path()).unwrap();
    assert!(done(&down).0.starts_with("refused: the VM is unreachable"));
}

#[test]
fn a_fetch_of_an_ended_run_the_vm_forgot_reports_the_laptop_s_record() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    let lock = w.store.placement_lock().unwrap();
    let mut r = w.store.record(&x.id).unwrap().unwrap();
    r.run = Some("v7".to_owned());
    snapshot::end(
        &w.store,
        &lock,
        &mut r,
        &crate::runner::state::State::outcome_only(Outcome::Timeout),
    )
    .unwrap();
    drop(lock);
    let other = Label {
        worktree: "/elsewhere".into(),
        ..label()
    };
    let out = tempfile::tempdir().unwrap();
    let got = fetch(&w.store, &mut w.vm, &other, &x.id, out.path()).unwrap();
    assert_eq!(done(&got), ("ended Timeout".to_owned(), 0));
}

#[test]
fn a_garbled_fetch_is_an_error() {
    decode(b"no newline").unwrap_err();
    decode(b"not json\n").unwrap_err();
    let reply = r#"{"job":{"state":"queued","run":"v1","entries":[]},"files":[{"attempt":1,"name":"tlc.log","len":10}]}"#;
    assert_eq!(
        decode(format!("{reply}\nshort").as_bytes()).unwrap_err(),
        "the files are cut short"
    );
    let whole = format!("{reply}\n0123456789");
    let (r, files) = decode(whole.as_bytes()).unwrap();
    assert_eq!((r.files.len(), files), (1, vec![&b"0123456789"[..]]));
}

#[test]
fn cancel_reports_the_run_the_agent_follows_not_the_id_s_latest() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    // v1 times out unseen, and the laptop's record has since moved on to a v2.
    let dir = w.vm.runner.store.attempt_dir(&x.id, "v1", 1);
    std::fs::write(dir.join("started"), "").unwrap();
    std::fs::write(dir.join("timed-out"), "").unwrap();
    w.vm.fake
        .gone(&crate::runner::verbs::container_name(&x.id, "v1", 1));
    let lock = w.store.placement_lock().unwrap();
    let mut r = w.store.record(&x.id).unwrap().unwrap();
    r.run = Some("v2".to_owned());
    w.store.write_record(&lock, &mut r).unwrap();
    drop(lock);
    let req = crate::proto::PreflightRequest {
        ids: vec![x.id.clone()],
        ..crate::proto::PreflightRequest::default()
    };
    w.vm.runner.status(&req).unwrap();
    let got = cancel(&w.store, &mut w.vm, &mut w.sups, &label(), &x.id).unwrap();
    assert_eq!(done(&got).0, "ended Timeout");
    assert_eq!(
        w.store.record(&x.id).unwrap().unwrap().run.as_deref(),
        Some("v2")
    );
}

#[test]
fn a_short_id_names_the_job_it_starts() {
    let mut w = world();
    let (x, y) = two(&mut w);
    let short = x.get(..12).unwrap();
    assert_eq!(resolve(&w.store, short).unwrap(), Ok(x.clone()));
    assert_eq!(resolve(&w.store, &y).unwrap(), Ok(y.clone()));
    assert_eq!(resolve(&w.store, "zz").unwrap(), Ok("zz".to_owned()));
    let common = x.chars().zip(y.chars()).take_while(|(a, b)| a == b).count();
    let both = x.get(..common).unwrap();
    assert!(resolve(&w.store, both)
        .unwrap()
        .unwrap_err()
        .contains("names 2 jobs"));
}

#[test]
fn a_cancel_of_an_ended_earlier_laptop_run_reports_it_and_touches_nothing() {
    let mut w = world();
    let id = crate::status::tests::moved_on_from_l1(&mut w);
    let got = cancel(&w.store, &mut w.vm, &mut w.sups, &label(), &id).unwrap();
    assert_eq!(done(&got), ("ended Ok".to_owned(), 0));
    assert_eq!(w.store.record(&id).unwrap().unwrap().state, State::Laptop);
    assert_eq!(w.sups.processes.killed, Vec::<u32>::new());
}

#[test]
fn resume_recovers_a_paused_run_and_refuses_otherwise() {
    let mut w = world();
    let (x, _) = two(&mut w);
    let refused = resume(&w.store, &mut w.vm, &label(), &x, None, None).unwrap();
    assert_eq!(done(&refused).0, "refused: it is not paused on the VM");
    // x stops for a pause, its checkpoint kept, and is settled paused.
    let runner = &w.vm.runner;
    let attempt = runner.store.attempt_dir(&x, "v1", 1);
    std::fs::write(attempt.join("started"), "").unwrap();
    std::fs::write(attempt.join("paused"), "").unwrap();
    let at = runner.store.checkpoint_dir(&x, "v1");
    std::fs::create_dir_all(at.join("tlc")).unwrap();
    std::fs::write(at.join("tlc/queue.chkpt"), "").unwrap();
    let kept = crate::runner::store::Checkpoint {
        workers: 16,
        fp: None,
        generated: None,
        attempt: Some(1),
    };
    crate::files::write_json(&at.join("checkpoint.json"), &kept).unwrap();
    w.vm.fake
        .gone(&crate::runner::verbs::container_name(&x, "v1", 1));
    let resumed = resume(&w.store, &mut w.vm, &label(), &x, Some(8192), None).unwrap();
    assert_eq!(done(&resumed), ("vm Running".to_owned(), 0));
    w.vm.down = true;
    let down = resume(&w.store, &mut w.vm, &label(), &x, None, None).unwrap();
    assert!(done(&down).0.starts_with("refused: the VM is unreachable"));
}

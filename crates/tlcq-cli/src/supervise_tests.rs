//! Tests of the laptop supervisor, with a script standing in for `java` and a
//! clock that jumps a poll at a time.

use std::cell::{Cell, RefCell};
use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::submit::tests::{job, submit_in, world, Local, World};

/// A clock whose every sleep moves it on by the time asked, after a real pause
/// short enough for the script to run, and runs `hook` once per sleep.
struct Ticks {
    now: Cell<u64>,
    sleeps: Cell<u32>,
    hook: RefCell<Box<dyn FnMut(u32)>>,
}

impl Ticks {
    fn new(hook: impl FnMut(u32) + 'static) -> Self {
        Self {
            now: Cell::new(0),
            sleeps: Cell::new(0),
            hook: RefCell::new(Box::new(hook)),
        }
    }
}

impl Clock for Ticks {
    fn now(&self) -> u64 {
        self.now.get()
    }

    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_millis(50));
        self.now.set(self.now.get() + secs);
        self.sleeps.set(self.sleeps.get() + 1);
        (self.hook.borrow_mut())(self.sleeps.get());
    }
}

/// A laptop job placed while the VM was unreachable, waiting for its
/// supervisor, and a `java` script running `body`.
fn placed(body: &str) -> (World, String, PathBuf) {
    placed_behind(&[], body)
}

/// The same, with slot tickets of the jobs `ahead` written first.
fn placed_behind(ahead: &[&str], body: &str) -> (World, String, PathBuf) {
    let mut w = world();
    let lock = w.store.placement_lock().unwrap();
    for other in ahead {
        w.store.write_ticket(&lock, other).unwrap();
    }
    drop(lock);
    w.vm.down = true;
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 1, false, "t1");
    w.vm.down = false;
    let java = w.dir.path().join("java");
    std::fs::write(&java, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&java, std::fs::Permissions::from_mode(0o755)).unwrap();
    (w, x.id, java)
}

/// One slot, a 5-minute cap, a minute's poll: a few polls to the cap.
fn settings() -> Settings {
    Settings {
        slots: 1,
        resources: Resources {
            workers: 1,
            heap_mb: 64,
            direct_mb: None,
            tlx_mem_mb: 64,
        },
        cap_secs: 300,
        disk_floor: 0,
        poll_secs: 60,
        limits: Limits::default(),
    }
}

fn supervise(
    store: &Store,
    java: PathBuf,
    remote: Local,
    clock: Ticks,
) -> Supervise<'_, Ticks, Local> {
    Supervise {
        store,
        java,
        jars: Vec::new(),
        tlx: PathBuf::from("tlx"),
        versions: crate::engine::test_versions("J"),
        settings: settings(),
        clock,
        remote,
        token: "move".to_owned(),
    }
}

/// A supervisor polling every second, so a script slow to start (the first
/// run of a new file is scanned) still finishes within the cap.
fn finishing(mut s: Supervise<'_, Ticks, Local>) -> Supervise<'_, Ticks, Local> {
    s.settings.poll_secs = 1;
    s
}

fn record(store: &Store, id: &str) -> JobRecord {
    store.record(id).unwrap().unwrap()
}

/// Set the job's `--vm-on-timeout`, as its submitter would have.
fn vm_on_timeout(store: &Store, id: &str) {
    let lock = store.placement_lock().unwrap();
    let mut r = record(store, id);
    r.info.as_mut().unwrap().vm_on_timeout = true;
    store.write_record(&lock, &mut r).unwrap();
}

const PROGRESS: &str = "Progress(2) at 2026-10-18 10:00:00: 30 states generated, 20 distinct states found, 4 states left on queue.";

#[test]
fn a_run_tlc_finishes_ends_with_its_verdict_and_frees_the_slot() {
    let (w, id, java) = placed(&format!("echo '{PROGRESS}'\nsleep 0.2\nexit 0"));
    let mut s = finishing(supervise(&w.store, java, w.vm, Ticks::new(|_| {})));
    s.run(&id, "l1").unwrap();
    let r = record(&w.store, &id);
    let result = r.result.unwrap();
    assert_eq!(
        (r.state, result.outcome, result.place.as_str()),
        (State::Ended, Outcome::Ok, "laptop")
    );
    assert_eq!(
        w.store.cached(&id).unwrap().map(|c| c.outcome),
        Some(Outcome::Ok)
    );
    let dir = w.store.run_dir(&id, "l1");
    let progress = Progress::read(&w.store, &id, "l1").unwrap();
    assert_eq!(progress.phase, Phase::Running);
    assert_eq!(progress.counters.map(|c| c.distinct), Some(20));
    assert!(dir.join("result.json").exists() && dir.join("tlc.log").exists());
    assert!(!dir.join("job").exists() && !dir.join("scratch").exists());
    assert_eq!(w.store.tickets().unwrap(), []);
}

#[test]
fn a_run_past_its_cap_is_stopped_and_ends_timeout() {
    let (w, id, java) = placed("exec sleep 30");
    let mut s = supervise(&w.store, java, w.vm, Ticks::new(|_| {}));
    s.run(&id, "l1").unwrap();
    let r = record(&w.store, &id);
    assert_eq!(r.result.unwrap().outcome, Outcome::Timeout);
    assert_eq!(r.elapsed_secs, 300);
    assert!(w.store.run_dir(&id, "l1").join("timed-out").exists());
}

#[test]
fn a_run_past_its_cap_with_vm_on_timeout_moves_to_the_vm_under_its_name() {
    let (w, id, java) = placed("exec sleep 30");
    vm_on_timeout(&w.store, &id);
    let runner = w.vm.runner.store.clone();
    let mut s = supervise(&w.store, java, w.vm, Ticks::new(|_| {}));
    s.run(&id, "l1").unwrap();
    let r = record(&w.store, &id);
    assert_eq!(
        (r.state, r.run.as_deref(), r.token),
        (State::Vm, Some("l1"), None)
    );
    assert_eq!((r.entries.len(), r.mirror.len()), (0, 1));
    let lock = runner.lock().unwrap();
    let state = runner.load(&lock).unwrap();
    let run = state.latest(&id).unwrap();
    assert_eq!(run.name, "l1");
    assert_eq!(run.info.expected, Some(Expected::Long));
    assert_eq!(run.info.laptop_secs, 300);
}

#[test]
fn a_move_the_vm_refuses_ends_error_and_one_never_answered_stays_moving() {
    let (w, id, java) = placed("exec sleep 30");
    vm_on_timeout(&w.store, &id);
    // A description the VM's checks refuse: its id no longer matches.
    let lock = w.store.placement_lock().unwrap();
    let mut r = record(&w.store, &id);
    r.info.as_mut().unwrap().tlc_opts = vec!["-deadlock".to_owned()];
    w.store.write_record(&lock, &mut r).unwrap();
    drop(lock);
    let mut s = supervise(&w.store, java, w.vm, Ticks::new(|_| {}));
    s.run(&id, "l1").unwrap();
    let result = record(&w.store, &id).result.unwrap();
    assert!(
        matches!(&result.outcome, Outcome::Error { reason: Some(r) } if r.starts_with("its move to the VM failed")),
        "{result:?}"
    );
    let (w, id, java) = placed("exec sleep 30");
    vm_on_timeout(&w.store, &id);
    let mut vm = w.vm;
    vm.lose_reply = true;
    let mut s = supervise(&w.store, java, vm, Ticks::new(|_| {}));
    s.run(&id, "l1").unwrap();
    let r = record(&w.store, &id);
    assert_eq!(
        (r.state, r.token.as_deref(), r.elapsed_secs),
        (State::Moving, Some("move"), 300)
    );
}

#[test]
fn a_run_waits_its_turn_behind_a_live_ticket_and_skips_a_stale_one() {
    // Ahead of it: one stale ticket, and one whose supervisor lives.
    let (w, id, java) = placed_behind(&["stale", "live"], "exit 0");
    let live = w.store.job_lock("live").unwrap();
    let held = RefCell::new(Some(live));
    let store = w.store.clone();
    let seen = std::rc::Rc::new(RefCell::new(Vec::new()));
    let seen_in_hook = seen.clone();
    let id_in_hook = id.clone();
    let clock = Ticks::new(move |n| {
        if n == 1 {
            seen_in_hook
                .borrow_mut()
                .push(Progress::read(&store, &id_in_hook, "l1").unwrap().phase);
            held.borrow_mut().take();
        }
    });
    let mut s = finishing(supervise(&w.store, java, w.vm, clock));
    s.run(&id, "l1").unwrap();
    assert_eq!(*seen.borrow(), [Phase::Slot { ahead: 1 }]);
    assert_eq!(record(&w.store, &id).result.unwrap().outcome, Outcome::Ok);
}

#[test]
fn a_run_below_the_disk_floor_waits_for_disk() {
    let (w, id, java) = placed("exit 0");
    let mut s = supervise(&w.store, java, w.vm, Ticks::new(|_| {}));
    s.settings.disk_floor = u64::MAX;
    assert!(matches!(s.look(&id).unwrap(), Some(Phase::Disk { .. })));
    s.settings.slots = 0;
    assert_eq!(s.look(&id).unwrap(), Some(Phase::Slot { ahead: 1 }));
}

#[test]
fn a_run_whose_record_moved_on_writes_its_result_and_leaves_the_record() {
    let (w, id, java) = placed("exit 0");
    let lock = w.store.placement_lock().unwrap();
    let mut r = record(&w.store, &id);
    r.run = Some("l2".to_owned());
    w.store.write_record(&lock, &mut r).unwrap();
    drop(lock);
    let mut s = finishing(supervise(&w.store, java, w.vm, Ticks::new(|_| {})));
    s.run(&id, "l1").unwrap();
    assert_eq!(record(&w.store, &id).state, State::Laptop);
    assert!(w.store.run_dir(&id, "l1").join("result.json").exists());
}

#[test]
fn a_job_without_a_record_fails_its_supervisor() {
    let (w, _, java) = placed("exit 0");
    let mut s = supervise(&w.store, java, w.vm, Ticks::new(|_| {}));
    s.run("nope", "l1").unwrap_err();
    let clock = AwakeClock::default();
    clock.sleep(0);
    assert!(clock.now() < 5);
}

#[test]
fn a_running_run_s_progress_gives_its_start_rates_and_time_left() {
    let later = PROGRESS
        .replace("Progress(2)", "Progress(3)")
        .replace("10:00:00", "10:00:01")
        .replace("30 states", "90 states");
    let (w, id, java) = placed(&format!(
        "echo '{PROGRESS}'\nsleep 0.5\necho '{later}'\nsleep 0.5\nexit 0"
    ));
    let mut s = finishing(supervise(&w.store, java, w.vm, Ticks::new(|_| {})));
    s.run(&id, "l1").unwrap();
    let p = Progress::read(&w.store, &id, "l1").unwrap();
    assert!(p.started_at.is_some());
    assert!(p.left_secs.is_some_and(|(l, cap)| l < cap && cap == 300));
    assert!(p.generated_per_min.is_some_and(|g| g > 0), "{p:?}");
    assert_eq!(p.queue_growth_permille, Some(0));
}

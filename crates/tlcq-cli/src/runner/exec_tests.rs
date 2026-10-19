//! Tests of the VM supervisor, with a shell script standing in for TLC and a
//! clock that jumps a minute per look.

use std::cell::Cell;
use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::commit::CommitRoute;
use crate::curve::Curve;
use crate::outcome::{Lane, Outcome};
use crate::runner::state::State;
use crate::runner::verbs::tests::Fake;
use crate::runner::verbs::tests::{job, runner, wire};
use crate::runner::verbs::Runner;

const PASS: &str = include_str!("../../tests/fixtures/tlc/pass.log");
const INVARIANT: &str = include_str!("../../tests/fixtures/tlc/invariant.log");

/// A clock that jumps ahead at every wait, once the script has written its
/// `ready` file (if it writes one), so its output lands in the first minute.
struct Jumpy(Cell<u64>, Option<PathBuf>);

impl Clock for Jumpy {
    fn now(&self) -> u64 {
        self.0.get()
    }

    fn sleep(&self, secs: u64) {
        for _ in 0..500 {
            if self.1.as_ref().is_none_or(|ready| ready.exists()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        self.0.set(self.0.get() + secs);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A big job's progress: the queue at half the distinct states.
const BIG: &str = "Progress(9) at 2026-10-04 14:52:12: 3,000,000 states generated (1 s/min), \
                   1,000,000 distinct states found (1 ds/min), 500,000 states left on queue.\n";

struct Setup {
    dir: tempfile::TempDir,
    runner: Runner<Fake>,
    id: String,
    machine: Machine,
}

/// A runner holding one job, running its attempt 1 in the E slot, and TLC
/// replaced by `script`.
fn setup(script: &str) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let (mut runner, _) = runner(&dir.path().join("vm"));
    let (a, tar) = job("A", CommitRoute::Queue);
    let id = a.id.clone();
    runner
        .commit(wire("t1", vec![(a, tar)], Vec::new()).as_slice())
        .unwrap();
    let tlc = dir.path().join("tlc.sh");
    std::fs::write(&tlc, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&tlc, std::fs::Permissions::from_mode(0o755)).unwrap();
    let machine = Machine {
        java: tlc,
        jars: vec![
            PathBuf::from("/jars/tla2tools.jar"),
            PathBuf::from("/jars/community.jar"),
        ],
        tlx: PathBuf::from("/bin/tlx"),
        tlx_mem_percent: 80,
        e_cpus: "16-31".to_owned(),
        p_cpus: "0-15".to_owned(),
        memory_mb: 128_000,
        reserve_mb: 4_000,
        heap_percent: 40,
        direct_percent: 50,
        slot_memory_mb: None,
    };
    Setup {
        dir,
        runner,
        id,
        machine,
    }
}

fn exec_with(s: &Setup, settings: Settings) -> AttemptResult {
    let e = Exec {
        store: &s.runner.store,
        machine: &s.machine,
        settings,
        limits: Limits::default(),
        clock: Jumpy(Cell::new(1_000), Some(s.dir.path().join("ready"))),
        poll_secs: 60,
    };
    e.run(&s.id, "v1", 1).unwrap();
    read_json(
        &s.runner
            .store
            .attempt_dir(&s.id, "v1", 1)
            .join("result.json"),
    )
    .unwrap()
    .unwrap()
}

fn exec(s: &Setup) -> AttemptResult {
    exec_with(s, Settings::default())
}

/// Settings whose caps stop a job after two minutes of running.
fn short_caps() -> Settings {
    Settings {
        slow_cap_secs: 120,
        max_cap_secs: 120,
        ..Settings::default()
    }
}

/// Rewrite the runner's state of the job's run.
fn edit(s: &Setup, change: impl FnOnce(&mut State)) {
    let lock = s.runner.store.lock().unwrap();
    let mut state = s.runner.store.load(&lock).unwrap();
    change(&mut state);
    s.runner.store.save(&lock, &state).unwrap();
}

fn fixture(s: &Setup, name: &str, text: &str) -> PathBuf {
    let path = s.dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn attempt_dir(s: &Setup) -> PathBuf {
    s.runner.store.attempt_dir(&s.id, "v1", 1)
}

/// TLC replaced by a script that prints `lines`, says it is ready, and waits
/// to be stopped.
fn waiting(s: &Setup, lines: &str) {
    let log = fixture(s, "lines.log", lines);
    std::fs::write(
        &s.machine.java,
        format!(
            "#!/bin/sh\ncat {}\ntouch {}\nexec sleep 30\n",
            log.display(),
            s.dir.path().join("ready").display()
        ),
    )
    .unwrap();
}

#[test]
fn tlc_exiting_on_its_own_gives_the_verdict() {
    let s = setup("");
    let log = fixture(&s, "pass.log", PASS);
    let args = s.dir.path().join("args");
    std::fs::write(
        &s.machine.java,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\ncat {}\ntouch {}\n",
            args.display(),
            log.display(),
            s.dir.path().join("ready").display()
        ),
    )
    .unwrap();
    let result = exec(&s);
    let finished = result.finished.unwrap();
    assert_eq!(
        (finished.outcome, finished.exit_status, finished.distinct),
        (Outcome::Ok, Some(0), Some(21_595))
    );
    assert_eq!(finished.place, "vm E");
    assert_eq!(
        (result.attempt, result.log.as_deref(), result.out_of_memory),
        (1, Some("tlc.log"), false)
    );
    assert!(result.started_at.is_some() && result.ended_at >= result.started_at);
    let attempt = attempt_dir(&s);
    assert!(attempt.join("started").exists());
    assert!(attempt.join("job/model/job.cfg").exists());
    let args = std::fs::read_to_string(args).unwrap();
    assert!(args.contains("-workers\n16\n"), "{args}");
    assert!(args.contains("-checkpoint\n10\n"), "{args}");
    assert!(args.contains(&format!(
        "-config\n{}",
        attempt.join("job/model/job.cfg").display()
    )));
    assert!(args.ends_with(&format!("{}\n", attempt.join("job/model/A.tla").display())));
}

#[test]
fn a_run_with_a_checkpoint_recovers_it_with_its_workers_and_fingerprints() {
    let s = setup("");
    let args = s.dir.path().join("args");
    // TLC recovers 100 states, then counts generated states from there.
    let log = fixture(
        &s,
        "recovered.log",
        &format!(
            "Recovery completed. 100 states examined. 5 states on queue.\n\
             Progress(3) at 2026-10-19 07:11:37: 150 states generated (150 s/min), \
             120 distinct states found (120 ds/min), 7 states left on queue.\n\
             Checkpointing completed at (2026-10-19 07:11:38)\n{PASS}"
        ),
    );
    std::fs::write(
        &s.machine.java,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\ncat {}\ntouch {}\n",
            args.display(),
            log.display(),
            s.dir.path().join("ready").display()
        ),
    )
    .unwrap();
    let at = s.runner.store.checkpoint_dir(&s.id, "v1");
    std::fs::create_dir_all(at.join("tlc")).unwrap();
    let record = Checkpoint {
        workers: 7,
        fp: Some(83),
        generated: Some(1000),
        attempt: None,
    };
    crate::files::write_json(&at.join("checkpoint.json"), &record).unwrap();
    let result = exec(&s);
    // The run's count carries on from the checkpoint's 1000, not from 100.
    let curve = Curve::read_in(&attempt_dir(&s)).unwrap().unwrap();
    assert_eq!(curve.points.first().map(|p| p.generated), Some(1050.0));
    assert_eq!(curve.last.map(|(g, _)| g), Some(51804.0 + 900.0));
    assert_eq!(result.finished.and_then(|f| f.generated), Some(51804 + 900));
    // Its checkpoint in place, over the recovered one, carries the run's count.
    assert_eq!(
        s.runner.store.checkpoint(&s.id, "v1").unwrap(),
        Some(Checkpoint {
            generated: Some(1050),
            ..record
        })
    );
    let args = std::fs::read_to_string(args).unwrap();
    let spec = attempt_dir(&s).join("job/model/A.tla");
    assert!(args.contains("-workers\n7\n"), "{args}");
    assert!(
        args.ends_with(&format!(
            "-recover\n{}\n-fp\n83\n{}\n",
            at.join("tlc").display(),
            spec.display()
        )),
        "{args}"
    );
    // Without the fingerprint function, only the checkpoint.
    let job = s.dir.path().join("job");
    let info = crate::runner::state::tests::info();
    let unknown = Checkpoint { fp: None, ..record };
    let (_, args) = s.machine.command(
        Slot::E,
        &info,
        false,
        &job,
        Path::new("/s"),
        Some((unknown, Path::new("/c"))),
    );
    let spec = job.join("model").join(format!("{}.tla", info.spec));
    assert!(
        args.ends_with(&[
            "-recover".to_owned(),
            "/c".to_owned(),
            spec.display().to_string()
        ]),
        "{args:?}"
    );
    assert!(!args.contains(&"-fp".to_owned()), "{args:?}");
}

#[test]
fn a_violation_the_header_does_not_expect_is_unexpected() {
    let s = setup("");
    let log = fixture(&s, "invariant.log", INVARIANT);
    std::fs::write(
        &s.machine.java,
        format!(
            "#!/bin/sh\ncat {}\ntouch {}\nexit 12\n",
            log.display(),
            s.dir.path().join("ready").display()
        ),
    )
    .unwrap();
    let finished = exec(&s).finished.unwrap();
    assert_eq!(
        (finished.outcome, finished.exit_status),
        (Outcome::Unexpected, Some(12))
    );
}

#[test]
fn a_big_job_becomes_max_in_place_and_runs_to_max_s_cap() {
    let s = setup("");
    waiting(&s, BIG);
    let result = exec(&s);
    assert_eq!(result.lanes.last().map(|d| d.to), Some(Lane::Max));
    // It ran on in its slot until max's cap stopped it.
    assert!(attempt_dir(&s).join("timed-out").exists());
    assert!(result.duration_secs >= Settings::default().max_cap_secs);
}

/// TLC replaced by a script that starts with fingerprint function 77, then
/// writes `checkpoint` lines (checkpointing into its scratch), then waits to
/// be stopped.
fn checkpointing(s: &Setup, checkpoint: &str) {
    let scratch = attempt_dir(s).join("scratch/26-10-19-07-10-36.000");
    std::fs::write(
        &s.machine.java,
        format!(
            "#!/bin/sh\necho 'Running breadth-first search Model-Checking with fp 77 and seed 1'\n\
             echo 'Progress(2) at 2026-10-19 07:40:35: 5000 states generated (1 s/min), \
             2000 distinct states found (1 ds/min), 300 states left on queue.'\n\
             sleep 0.2\nmkdir -p {scratch}\ntouch {scratch}/queue.chkpt\necho '{checkpoint}'\n\
             touch {ready}\nexec sleep 30\n",
            scratch = scratch.display(),
            ready = s.dir.path().join("ready").display()
        ),
    )
    .unwrap();
}

#[test]
fn a_pause_stops_the_job_at_its_next_checkpoint_keeping_it() {
    let s = setup("");
    checkpointing(&s, "Checkpointing completed at (2026-10-19 07:40:36)");
    let attempt = attempt_dir(&s);
    std::fs::write(attempt.join("pause"), "").unwrap();
    let result = exec(&s);
    assert_eq!(result.finished, None);
    assert!(attempt.join("paused").exists());
    // Its checkpoint moved out of the scratch for the resume, this attempt's.
    assert_eq!(
        s.runner.store.checkpoint(&s.id, "v1").unwrap(),
        Some(Checkpoint {
            workers: 16,
            fp: Some(77),
            generated: Some(5000),
            attempt: Some(1),
        })
    );
    let tlc = s.runner.store.checkpoint_dir(&s.id, "v1").join("tlc");
    assert!(tlc.join("queue.chkpt").exists());
    assert!(!attempt.join("scratch/26-10-19-07-10-36.000").exists());
}

#[test]
fn a_pause_waits_for_a_checkpoint_being_written_and_never_comes_after_the_search() {
    // A checkpoint started, never completed: no pause before the cap.
    for line in [
        "Checkpointing of run /s",
        "Checking 2 branches of temporal properties for the complete state space with 9 total distinct states",
    ] {
        let s = setup("");
        checkpointing(&s, line);
        let attempt = attempt_dir(&s);
        std::fs::write(attempt.join("pause"), "").unwrap();
        exec_with(&s, short_caps());
        assert!(!attempt.join("paused").exists(), "{line}");
        assert!(attempt.join("timed-out").exists(), "{line}");
        assert!(!s.runner.store.checkpoint_dir(&s.id, "v1").exists());
        // The page learns no checkpoint is coming once the search has ended.
        assert_eq!(attempt.join("searched").exists(), line.contains("temporal"), "{line}");
    }
}

#[test]
fn a_frozen_job_unpaused_for_its_pause_with_no_checkpoint_stops_at_once() {
    let unpaused = |s: &Setup| {
        for (event, at) in [("freeze", 900), ("unpause", 990)] {
            s.runner
                .store
                .log_frozen(&s.id, "v1", 1, event, at)
                .unwrap();
        }
        std::fs::write(attempt_dir(s).join("pause"), "").unwrap();
    };
    // Never a checkpoint: it stops as paused before its first one.
    let s = setup("");
    waiting(&s, "");
    unpaused(&s);
    exec(&s);
    let attempt = attempt_dir(&s);
    assert!(attempt.join("no-checkpoint").exists() && !attempt.join("paused").exists());
}

#[test]
fn keeping_a_checkpoint_moves_one_from_the_scratch_or_counts_on_one_in_place() {
    let s = setup("exit 0");
    let e = Exec {
        store: &s.runner.store,
        machine: &s.machine,
        settings: Settings::default(),
        limits: Limits::default(),
        clock: Jumpy(Cell::new(1_000), None),
        poll_secs: 60,
    };
    let attempt = attempt_dir(&s);
    // A recovered checkpoint stays where it is; with none in the scratch, or
    // no scratch, nothing is kept.
    let at = s.runner.store.checkpoint_dir(&s.id, "v1");
    for (tlc, scratch) in [(true, true), (false, true), (false, false)] {
        s.runner.store.delete_checkpoint(&s.id, "v1").unwrap();
        s.runner.store.delete_scratch(&s.id, "v1", 1).unwrap();
        if tlc {
            std::fs::create_dir_all(at.join("tlc")).unwrap();
        }
        if scratch {
            std::fs::create_dir_all(attempt.join("scratch/empty")).unwrap();
        }
        e.keep_checkpoint(&s.id, "v1", 1, Slot::E, &attempt, Some(42))
            .unwrap();
        assert_eq!(at.join("tlc").exists(), tlc);
        assert!(!at.join("checkpoint.json").exists());
    }
    // A recovered one, TLC checkpointing into it in place: its count moves
    // on, and the attempt that paused now holds it.
    let recovered = Checkpoint {
        workers: 7,
        fp: Some(83),
        generated: Some(1000),
        attempt: Some(1),
    };
    std::fs::create_dir_all(at.join("tlc")).unwrap();
    crate::files::write_json(&at.join("checkpoint.json"), &recovered).unwrap();
    e.keep_checkpoint(&s.id, "v1", 2, Slot::P, &attempt, Some(5000))
        .unwrap();
    assert_eq!(
        s.runner.store.checkpoint(&s.id, "v1").unwrap(),
        Some(Checkpoint {
            generated: Some(5000),
            attempt: Some(2),
            ..recovered
        })
    );
}

#[test]
fn frozen_time_counts_toward_no_cap_or_duration() {
    let s = setup("");
    waiting(&s, "");
    let id = s.id.clone();
    edit(&s, |state| {
        state.runs.get_mut(&id).unwrap()[0].info.timeout_secs = Some(120);
    });
    // Frozen for ten minutes from its first look on.
    for (event, at) in [("freeze", 1000), ("thaw e", 1600)] {
        s.runner
            .store
            .log_frozen(&s.id, "v1", 1, event, at)
            .unwrap();
    }
    let result = exec(&s);
    assert!(attempt_dir(&s).join("timed-out").exists());
    // Two minutes of running, not the twelve on the clock.
    assert!(
        (120..=180).contains(&result.duration_secs),
        "{}",
        result.duration_secs
    );
    assert!(result.ended_at.unwrap() - result.started_at.unwrap() >= 720);
}

#[test]
fn a_cap_stops_tlc_and_so_does_a_cancel_marker_without_a_marker_of_its_own() {
    let s = setup("");
    let ready = s.dir.path().join("ready");
    std::fs::write(
        &s.machine.java,
        format!("#!/bin/sh\ntouch {}\nexec sleep 30\n", ready.display()),
    )
    .unwrap();
    let id = s.id.clone();
    edit(&s, |state| {
        state.runs.get_mut(&id).unwrap()[0].info.timeout_secs = Some(120);
    });
    let result = exec(&s);
    assert_eq!(result.finished, None);
    let attempt = attempt_dir(&s);
    assert!(attempt.join("timed-out").exists());
    std::fs::remove_file(attempt.join("timed-out")).unwrap();
    std::fs::write(attempt.join("cancel.json"), "{}").unwrap();
    // TLC would run 30 s more; the cancel marker stops it at the next look,
    // and no stop marker is written over the cancel.
    let started = std::time::Instant::now();
    let result = exec(&s);
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert!(!attempt.join("timed-out").exists());
    assert_eq!(result.finished, None);
}

#[test]
fn tlc_exiting_on_its_own_keeps_its_verdict_even_with_a_stop_marker_written() {
    let s = setup("");
    let log = fixture(&s, "pass.log", PASS);
    let ready = s.dir.path().join("ready");
    std::fs::write(
        &s.machine.java,
        format!(
            "#!/bin/sh\ncat {}\ntouch {}\n",
            log.display(),
            ready.display()
        ),
    )
    .unwrap();
    // The attempt already holds a paused marker, as a stop that lost the race
    // to TLC's own exit leaves: the verdict is still written.
    std::fs::write(attempt_dir(&s).join("paused"), "").unwrap();
    let finished = exec(&s).finished.unwrap();
    assert_eq!(finished.outcome, Outcome::Ok);
}

#[test]
fn an_unknown_attempt_or_a_job_without_a_configuration_is_an_error() {
    let s = setup("exit 0");
    let e = Exec {
        store: &s.runner.store,
        machine: &s.machine,
        settings: Settings::default(),
        limits: Limits::default(),
        clock: Jumpy(Cell::new(0), None),
        poll_secs: 60,
    };
    std::fs::create_dir_all(s.runner.store.attempt_dir(&s.id, "v1", 7)).unwrap();
    e.run(&s.id, "v1", 7).unwrap_err();
    let tar = tar::Builder::new(Vec::new()).into_inner().unwrap();
    extract(&tar, s.dir.path(), Limits::default()).unwrap_err();
    extract(b"not a tarball", s.dir.path(), Limits::default()).unwrap_err();
}

#[test]
fn a_command_carries_the_libraries_the_options_and_lncheck_for_max() {
    let s = setup("");
    let job = s.dir.path().join("job");
    std::fs::create_dir_all(job.join("lib/0")).unwrap();
    std::fs::create_dir_all(job.join("lib/1")).unwrap();
    let mut info = crate::runner::state::tests::info();
    info.tlc_opts = vec!["-deadlock".to_owned()];
    let (_, args) = s
        .machine
        .command(Slot::P, &info, true, &job, Path::new("/s"), None);
    let libs = format!(
        "-DTLA-Library={}:{}",
        job.join("lib/0").display(),
        job.join("lib/1").display()
    );
    assert!(args.contains(&libs));
    let text = args.join(" ");
    assert!(
        text.contains("-workers 16 ") && text.contains("-lncheck final -deadlock -config"),
        "{text}"
    );
    assert!(
        text.starts_with("-Xmx24800m -XX:+UseParallelGC -XX:MaxDirectMemorySize=31000m"),
        "{text}"
    );
    assert!(text.contains("-checkpoint 10 -fpmem 1.0"), "{text}");
    // On the heap: no off-heap fingerprint set, no `-fpmem`.
    let heap = Resources {
        workers: 8,
        heap_mb: 8192,
        direct_mb: None,
        tlx_mem_mb: 8192,
    };
    let text = tlc_args(&s.machine.jars, heap, &info, false, &job, Path::new("/s")).join(" ");
    assert!(
        text.starts_with("-Xmx8192m -XX:+UseParallelGC -DTLA-Library")
            && text.contains("-workers 8 ")
            && !text.contains("fpmem")
            && !text.contains("lncheck"),
        "{text}"
    );
    let none = Machine {
        e_cpus: String::new(),
        ..s.machine.clone()
    };
    assert_eq!(none.resources(Slot::E, false).0, 1);
    assert_eq!(s.machine.resources(Slot::P, false), (16, 62_000));
    // The E slot alone, with the P slot closed, gets all of it.
    assert_eq!(s.machine.resources(Slot::E, true), (16, 124_000));
    // In an attempt's container, the cap the runner gave it is its memory.
    let capped = Machine {
        slot_memory_mb: Some(50_000),
        ..s.machine.clone()
    };
    assert_eq!(capped.resources(Slot::E, false), (16, 50_000));
}

#[test]
fn a_cpu_list_counts_its_ranges_and_single_cpus() {
    assert_eq!(cpu_count("16-31"), 16);
    assert_eq!(cpu_count("0-3,8, 10-11"), 7);
    assert_eq!(cpu_count("x,5-2,4"), 1);
    assert_eq!(cpu_count(""), 0);
}

#[test]
fn a_stop_marker_is_written_only_without_a_cancel_marker() {
    let s = setup("exit 0");
    let e = Exec {
        store: &s.runner.store,
        machine: &s.machine,
        settings: Settings::default(),
        limits: Limits::default(),
        clock: Jumpy(Cell::new(0), None),
        poll_secs: 60,
    };
    let dir = attempt_dir(&s);
    // A run with no checkpoint kept has no record to carry a count.
    e.note_checkpoint(&s.id, "v1", Some(5)).unwrap();
    assert!(!s.runner.store.checkpoint_dir(&s.id, "v1").exists());
    // A pause withdrawn (keep running) before the marker stops nothing.
    assert!(!e.mark(&dir, Stop::Paused).unwrap());
    assert!(!dir.join("paused").exists());
    std::fs::write(dir.join("pause"), "").unwrap();
    for (stop, name) in [
        (Stop::Timeout, "timed-out"),
        (Stop::Paused, "paused"),
        (Stop::NoCheckpoint, "no-checkpoint"),
    ] {
        assert!(e.mark(&dir, stop).unwrap());
        assert!(dir.join(name).exists());
    }
    std::fs::write(dir.join("cancel.json"), "{}").unwrap();
    std::fs::remove_file(dir.join("paused")).unwrap();
    assert!(!e.mark(&dir, Stop::Paused).unwrap());
    assert!(!dir.join("paused").exists());
}

#[test]
fn the_libraries_are_the_lib_directories_the_job_holds_even_with_a_gap() {
    let s = setup("");
    let job = s.dir.path().join("job");
    std::fs::create_dir_all(job.join("lib/1")).unwrap();
    std::fs::create_dir_all(job.join("lib/10")).unwrap();
    std::fs::create_dir_all(job.join("lib/2")).unwrap();
    std::fs::write(job.join("lib/notes"), "").unwrap();
    let info = crate::runner::state::tests::info();
    let (_, args) = s
        .machine
        .command(Slot::E, &info, false, &job, Path::new("/s"), None);
    let lib = |n: &str| job.join("lib").join(n).display().to_string();
    let want = format!("-DTLA-Library={}:{}:{}", lib("1"), lib("2"), lib("10"));
    assert!(args.contains(&want), "{args:?}");
}

#[test]
fn a_tlx_job_runs_tlx_with_its_own_options() {
    let s = setup("");
    let job = s.dir.path().join("job");
    std::fs::create_dir_all(job.join("lib/0")).unwrap();
    std::fs::create_dir_all(job.join("lib/1")).unwrap();
    let mut info = crate::runner::state::tests::info();
    info.engine = Engine::Tlx;
    info.tlc_opts = vec!["-deadlock".to_owned()];
    // A max job in the E slot: its 16 CPUs, 80% of half of 124,000 MB.
    let (program, args) = s
        .machine
        .command(Slot::E, &info, true, &job, Path::new("/s"), None);
    assert_eq!(program, PathBuf::from("/bin/tlx"));
    let model = job.join("model");
    let want = [
        "check".to_owned(),
        model.join("M.tla").display().to_string(),
        "-config".to_owned(),
        model.join("job.cfg").display().to_string(),
        "-workers".to_owned(),
        "16".to_owned(),
        "-maxmem".to_owned(),
        "49600M".to_owned(),
        "-metadir".to_owned(),
        "/s".to_owned(),
        "-checkpoint".to_owned(),
        "10".to_owned(),
        "-stats".to_owned(),
        "-I".to_owned(),
        job.join("lib/0").display().to_string(),
        "-I".to_owned(),
        job.join("lib/1").display().to_string(),
        "-deadlock".to_owned(),
    ];
    assert_eq!(args, want);
    // None of TLC's own options.
    for tlc_only in ["-lncheck", "-fp", "-fpmem", "-noGenerateSpecTE", "-cp"] {
        assert!(
            !args.iter().any(|a| a == tlc_only),
            "{tlc_only} in {args:?}"
        );
    }
    assert!(
        !args
            .iter()
            .any(|a| a.starts_with("-X") || a.starts_with("-D")),
        "{args:?}"
    );
}

#[test]
fn a_tlx_job_recovers_with_the_slot_s_workers_and_no_fingerprint_function() {
    let mut s = setup("");
    s.machine.p_cpus = "0-7".to_owned();
    let job = s.dir.path().join("job");
    let mut info = crate::runner::state::tests::info();
    info.engine = Engine::Tlx;
    let checkpoint = Checkpoint {
        workers: 21,
        fp: Some(83),
        generated: Some(1000),
        attempt: Some(1),
    };
    let (_, args) = s.machine.command(
        Slot::P,
        &info,
        true,
        &job,
        Path::new("/s"),
        Some((checkpoint, Path::new("/c"))),
    );
    let text = args.join(" ");
    // The P slot's 8 CPUs, not the 21 workers the checkpoint was written with.
    assert!(text.contains("-workers 8 "), "{text}");
    assert!(text.ends_with("-recover /c"), "{text}");
    assert!(!text.contains("-fp"), "{text}");
}

/// The job on tlx, which the script stands in for.
fn on_tlx(s: &mut Setup) {
    edit(s, |state| {
        for r in state.runs.values_mut().flatten() {
            r.info.engine = Engine::Tlx;
        }
    });
    s.machine.tlx = s.machine.java.clone();
}

#[test]
fn a_paused_tlx_job_keeps_tlx_s_metadir() {
    let mut s = setup("");
    on_tlx(&mut s);
    let scratch = attempt_dir(&s).join("scratch");
    std::fs::write(
        &s.machine.tlx,
        format!(
            "#!/bin/sh\necho 'Running breadth-first search Model-Checking with fp 0 and seed 0'\n\
             echo 'Progress(2) at 2026-10-19 07:40:35: 5,000 states generated (1 s/min), \
             2,000 distinct states found (1 ds/min), 300 states left on queue.'\n\
             sleep 0.2\nmkdir -p {scratch}/checkpoint\ntouch {scratch}/checkpoint/MANIFEST\n\
             echo 'Checkpointing completed at (2026-10-19 07:40:36)'\n\
             touch {ready}\nexec sleep 30\n",
            scratch = scratch.display(),
            ready = s.dir.path().join("ready").display()
        ),
    )
    .unwrap();
    std::fs::write(attempt_dir(&s).join("pause"), "").unwrap();
    exec(&s);
    assert!(attempt_dir(&s).join("paused").exists());
    // tlx's whole metadir is kept, with no fingerprint function.
    assert_eq!(
        s.runner.store.checkpoint(&s.id, "v1").unwrap(),
        Some(Checkpoint {
            workers: 16,
            fp: None,
            generated: Some(5000),
            attempt: Some(1),
        })
    );
    let kept = s.runner.store.checkpoint_dir(&s.id, "v1").join("tlc");
    assert!(kept.join("checkpoint/MANIFEST").exists());
    assert!(!scratch.exists());
    // With the binary that wrote it, the only one that recovers it.
    assert_eq!(
        std::fs::read(kept.with_file_name("tlx")).unwrap(),
        std::fs::read(&s.machine.tlx).unwrap()
    );
}

#[test]
fn a_tlx_recovery_takes_tlx_s_counts_as_they_come() {
    let mut s = setup("");
    on_tlx(&mut s);
    let args = s.dir.path().join("args");
    // tlx's counts after a recovery include the checkpoint's 1000.
    let log = fixture(
        &s,
        "recovered.log",
        &format!(
            "Recovery completed. 100 states examined. 5 states on queue.\n\
             Progress(3) at 2026-10-19 07:11:37: 1,050 states generated (150 s/min), \
             120 distinct states found (120 ds/min), 7 states left on queue.\n\
             Checkpointing completed at (2026-10-19 07:11:38)\n{PASS}"
        ),
    );
    std::fs::write(
        &s.machine.tlx,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\ncat {}\ntouch {}\n",
            args.display(),
            log.display(),
            s.dir.path().join("ready").display()
        ),
    )
    .unwrap();
    let at = s.runner.store.checkpoint_dir(&s.id, "v1");
    std::fs::create_dir_all(at.join("tlc")).unwrap();
    let record = Checkpoint {
        workers: 7,
        fp: None,
        generated: Some(1000),
        attempt: None,
    };
    crate::files::write_json(&at.join("checkpoint.json"), &record).unwrap();
    // The recovery runs the binary kept beside the checkpoint, not the image's.
    std::fs::rename(&s.machine.tlx, at.join("tlx")).unwrap();
    s.machine.tlx = s.dir.path().join("newer-tlx");
    exec(&s);
    let curve = Curve::read_in(&attempt_dir(&s)).unwrap().unwrap();
    assert_eq!(curve.points.first().map(|p| p.generated), Some(1050.0));
    let args = std::fs::read_to_string(args).unwrap();
    assert!(args.starts_with("check\n"), "{args}");
    assert!(
        args.ends_with(&format!("-recover\n{}\n", at.join("tlc").display())),
        "{args}"
    );
    assert!(!args.contains("-workers\n7\n"), "{args}");
}

#[test]
fn a_tlx_run_refused_before_its_version_line_is_still_tlx_s() {
    let mut s = setup("");
    on_tlx(&mut s);
    std::fs::write(
        &s.machine.tlx,
        format!(
            "#!/bin/sh\necho 'Error: cannot recover from /c: the tlx build differs'\ntouch {}\nexit 255\n",
            s.dir.path().join("ready").display()
        ),
    )
    .unwrap();
    let result = exec(&s);
    let verdict = result.finished.unwrap();
    assert_eq!(
        verdict.outcome,
        Outcome::Error {
            reason: Some("cannot recover from /c: the tlx build differs".to_owned())
        }
    );
}

#[test]
fn a_tlx_run_short_of_memory_for_liveness_says_what_to_resubmit_with() {
    let mut s = setup("");
    on_tlx(&mut s);
    std::fs::write(
        &s.machine.tlx,
        format!(
            "#!/bin/sh\necho 'tlx version 0.1.0'\necho 'Error: liveness needs more memory: with the liveness automaton and product (2 nodes, 3 edges so far), the process needs about 48049 MiB, over the memory budget of 43504 MiB (-maxmem); rerun with -maxmem 48064 or more.'\ntouch {}\nexit 255\n",
            s.dir.path().join("ready").display()
        ),
    )
    .unwrap();
    let verdict = exec(&s).finished.unwrap();
    assert!(
        matches!(verdict.outcome, Outcome::Error { .. }),
        "{verdict:?}"
    );
    // 48049 MiB as tlx's 80% of an attempt: 59 GB.
    assert_eq!(
        verdict.note.as_deref(),
        Some("needs more memory: resubmit with --memory 59G")
    );
}

#[test]
fn a_pause_lands_by_what_the_log_has_said() {
    let mut log = LogState::default();
    // Nothing yet: a running job waits, one unpaused from frozen stops bare.
    assert_eq!(log.pause_stop(false), None);
    assert_eq!(log.pause_stop(true), Some(Stop::NoCheckpoint));
    // A checkpoint being written is waited for.
    log.line("Checkpointing of run /s", Some(10));
    assert_eq!(log.pause_stop(true), None);
    log.line("Checkpointing completed at (2026-10-19 07:40:36)", Some(42));
    assert_eq!(
        (log.pause_stop(false), log.generated),
        (Some(Stop::Paused), Some(42))
    );
    // Later looks: a running job waits for the next; an unpaused one stops.
    log.just = false;
    assert_eq!(log.pause_stop(false), None);
    assert_eq!(log.pause_stop(true), Some(Stop::Paused));
    // After the search no checkpoint comes: a running job ends on its own. (One
    // unpaused, in no slot, would stop at once, but a pause is refused by now.)
    log.line(
        "Checking temporal properties for the complete state space with 3 total distinct states",
        None,
    );
    log.just = true;
    assert_eq!(log.pause_stop(false), None);
    assert_eq!(log.pause_stop(true), Some(Stop::Paused));
    // A recovered checkpoint is one to keep, with its count, from the start.
    let recovered = Checkpoint {
        workers: 16,
        fp: None,
        generated: Some(1000),
        attempt: Some(1),
    };
    let mut log = LogState::recovering(Some(recovered));
    assert_eq!(
        (log.pause_stop(true), log.generated),
        (Some(Stop::Paused), Some(1000))
    );
    // A checkpoint before its first progress line keeps the recovered count.
    log.line("Checkpointing completed at (2026-10-19 07:40:36)", None);
    assert_eq!(log.generated, Some(1000));
    assert_eq!(
        LogState::recovering(None).pause_stop(true),
        Some(Stop::NoCheckpoint)
    );
}

#[test]
fn an_engine_killed_by_a_signal_leaves_the_signal_and_no_verdict() {
    let s = setup("");
    std::fs::write(
        &s.machine.java,
        format!(
            "#!/bin/sh\ntouch {}\nkill -9 $$\n",
            s.dir.path().join("ready").display()
        ),
    )
    .unwrap();
    let result = exec(&s);
    assert_eq!((result.finished, result.signal), (None, Some(9)));
}

#[test]
fn a_long_run_whose_engine_dies_keeps_its_checkpoint_and_a_short_one_does_not() {
    // RS_pack_zombie2: the kernel killed tlx for memory at a level boundary.
    let killed = |s: &Setup| {
        let scratch = attempt_dir(s).join("scratch");
        format!(
            "#!/bin/sh\nmkdir -p {scratch}/checkpoint\ntouch {scratch}/checkpoint/MANIFEST\n\
             echo 'Checkpointing completed at (2026-10-19 07:40:36)'\n\
             touch {ready}\nkill -9 $$\n",
            scratch = scratch.display(),
            ready = s.dir.path().join("ready").display()
        )
    };
    let mut s = setup("");
    on_tlx(&mut s);
    std::fs::write(&s.machine.tlx, killed(&s)).unwrap();
    exec(&s);
    assert_eq!(s.runner.store.checkpoint(&s.id, "v1").unwrap(), None);
    // An hour in, by its earlier attempts' time: kept, with the binary.
    let mut s = setup("");
    on_tlx(&mut s);
    std::fs::write(&s.machine.tlx, killed(&s)).unwrap();
    let so_far = AttemptResult {
        duration_secs: KEEP_AFTER_SECS,
        ..AttemptResult::default()
    };
    write_json(&attempt_dir(&s).join("result.json"), &so_far).unwrap();
    let result = exec(&s);
    assert_eq!((result.finished, result.signal), (None, Some(9)));
    let kept = s.runner.store.checkpoint(&s.id, "v1").unwrap().unwrap();
    assert_eq!(kept.attempt, Some(1));
    let at = s.runner.store.checkpoint_dir(&s.id, "v1");
    assert!(at.join("tlc/checkpoint/MANIFEST").exists() && at.join("tlx").exists());
    // An engine's error keeps it too; a verdict does not.
    for (exit, keeps) in [("exit 255", true), ("exit 0", false)] {
        let mut s = setup("");
        on_tlx(&mut s);
        let script = killed(&s).replace("kill -9 $$", exit);
        std::fs::write(&s.machine.tlx, script).unwrap();
        write_json(&attempt_dir(&s).join("result.json"), &so_far).unwrap();
        exec(&s);
        assert_eq!(
            s.runner.store.checkpoint(&s.id, "v1").unwrap().is_some(),
            keeps,
            "{exit}"
        );
    }
}

#[test]
fn a_tlx_run_that_stopped_itself_out_of_memory_says_what_to_ask_for() {
    let mut s = setup("");
    on_tlx(&mut s);
    s.machine.slot_memory_mb = Some(59_904);
    std::fs::write(
        &s.machine.tlx,
        format!(
            "#!/bin/sh\necho 'tlx version 0.1.0'\necho 'Error: The search stopped: out of memory: the process holds 57344 MiB, within 3744 MiB of the 59904 MiB it can have, past its budget of 47923 MiB (-maxmem)'\ntouch {}\nexit 255\n",
            s.dir.path().join("ready").display()
        ),
    )
    .unwrap();
    let verdict = exec(&s).finished.unwrap();
    assert!(
        matches!(&verdict.outcome, Outcome::Error { reason: Some(r) } if r.starts_with("The search stopped: out of memory")),
        "{verdict:?}"
    );
    // The most one job gets: 128000 MB less the 4000 MB reserve, 121 GB.
    assert_eq!(
        verdict.note.as_deref(),
        Some("needs more memory than its 59904 MB: ask for up to --memory 121G, the most one job gets on the VM")
    );
}

#[test]
fn a_long_run_stopped_at_its_cap_keeps_its_checkpoint() {
    let mut s = setup("");
    on_tlx(&mut s);
    let scratch = attempt_dir(&s).join("scratch");
    std::fs::write(
        &s.machine.tlx,
        format!(
            "#!/bin/sh\nmkdir -p {scratch}/checkpoint\ntouch {scratch}/checkpoint/MANIFEST\n\
             echo 'Checkpointing completed at (2026-10-19 07:40:36)'\ntouch {ready}\nexec sleep 30\n",
            scratch = scratch.display(),
            ready = s.dir.path().join("ready").display()
        ),
    )
    .unwrap();
    let so_far = AttemptResult {
        duration_secs: KEEP_AFTER_SECS,
        ..AttemptResult::default()
    };
    write_json(&attempt_dir(&s).join("result.json"), &so_far).unwrap();
    exec_with(&s, short_caps());
    assert!(attempt_dir(&s).join("timed-out").exists());
    assert_eq!(
        s.runner
            .store
            .checkpoint(&s.id, "v1")
            .unwrap()
            .map(|c| c.attempt),
        Some(Some(1))
    );
}

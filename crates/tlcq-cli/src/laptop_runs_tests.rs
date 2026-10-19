//! Tests of the laptop's runs: launch, cancel and the orphan rule, with
//! supervisors that only hold their job's lock.

use super::*;
use crate::history::History;
use crate::store::State;
use crate::submit::tests::{Procs, Sups};

fn runs() -> Sups {
    LaptopRuns {
        processes: Procs::default(),
    }
}

/// A laptop job `a` on run `l1`, its run directory holding `files`, with a
/// ticket and a scratch directory.
fn laptop_job(store: &Store, lock: &PlacementLock, files: &[(&str, &str)]) -> JobRecord {
    let mut r = JobRecord::new("a", State::Laptop);
    r.run = Some("l1".to_owned());
    r.info = Some(crate::runner::state::tests::info());
    store.write_record(lock, &mut r).unwrap();
    store.write_ticket(lock, "a").unwrap();
    let dir = store.run_dir("a", "l1");
    std::fs::create_dir_all(dir.join("scratch")).unwrap();
    std::fs::create_dir_all(dir.join("job/model")).unwrap();
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
    r
}

fn outcome_of(store: &Store) -> Outcome {
    let r = store.record("a").unwrap().unwrap();
    assert_eq!(r.state, State::Ended);
    r.result.unwrap().outcome
}

#[test]
fn an_orphan_ends_by_its_markers_and_leaves_no_scratch_or_ticket() {
    let progress = r#"{"phase": {"phase": "running"}, "elapsed_secs": 400, "counters": null}"#;
    let ok = serde_json::to_string(&laptop_result(Outcome::Ok, 30)).unwrap();
    let cases: [(&[(&str, &str)], Outcome); 4] = [
        (
            &[
                ("cancel.json", r#"{"by": "w2", "replaced_by": "b"}"#),
                ("timed-out", ""),
            ],
            Outcome::Cancelled {
                by: "w2".to_owned(),
                replaced_by: Some("b".to_owned()),
            },
        ),
        (
            &[("timed-out", ""), ("progress.json", progress)],
            Outcome::Timeout,
        ),
        (
            &[],
            Outcome::Error {
                reason: Some("its supervisor died (the laptop restarted?)".to_owned()),
            },
        ),
        (&[("result.json", &ok)], Outcome::Ok),
    ];
    for (files, want) in cases {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let lock = store.placement_lock().unwrap();
        let mut r = laptop_job(&store, &lock, files);
        orphan(&store, &lock, &mut r).unwrap();
        assert_eq!(outcome_of(&store), want);
        let run_dir = store.run_dir("a", "l1");
        assert!(!run_dir.join("scratch").exists() && !run_dir.join("job").exists());
        assert_eq!(store.tickets().unwrap(), []);
        let written: JobResult = read_json(&run_dir.join("result.json")).unwrap().unwrap();
        assert_eq!(written.outcome, want);
        assert_eq!(written.place, "laptop");
        let history = History::load(&store.history()).unwrap();
        assert_eq!(history.records().len(), 1);
    }
}

#[test]
fn a_timed_out_orphan_records_the_awake_time_its_supervisor_last_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lock = store.placement_lock().unwrap();
    let progress = r#"{"phase": {"phase": "running"}, "elapsed_secs": 400, "counters": null}"#;
    let mut r = laptop_job(
        &store,
        &lock,
        &[("timed-out", ""), ("progress.json", progress)],
    );
    orphan(&store, &lock, &mut r).unwrap();
    let r = store.record("a").unwrap().unwrap();
    assert_eq!(
        (r.elapsed_secs, r.result.unwrap().duration_secs),
        (400, 400)
    );
    let history = History::load(&store.history()).unwrap();
    let record = &history.records()[0];
    assert!(record.lower_bound && record.duration_secs == 400);
}

#[test]
fn a_launch_holds_the_lock_and_a_ticket_and_a_cancel_kills_and_ends_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lock = store.placement_lock().unwrap();
    let mut sups = runs();
    let mut r = JobRecord::new("a", State::Laptop);
    r.run = Some("l1".to_owned());
    sups.launch(&store, &lock, &mut r, b"tar").unwrap();
    assert!(store.job_lock_held("a").unwrap());
    assert_eq!(store.tickets().unwrap(), [(1, "a".to_owned())]);
    assert_eq!(std::fs::read(store.tarball("a")).unwrap(), b"tar");
    let pid = std::fs::read_to_string(store.run_dir("a", "l1").join("pid")).unwrap();
    assert_eq!(pid, "1000");
    let marker = CancelMarker {
        by: "w1".to_owned(),
        replaced_by: None,
    };
    sups.cancel(&store, &lock, &mut r, &marker).unwrap();
    assert_eq!(sups.processes.killed, [1000]);
    assert!(!store.job_lock_held("a").unwrap());
    assert!(matches!(outcome_of(&store), Outcome::Cancelled { by, .. } if by == "w1"));
    assert_eq!(store.tickets().unwrap(), []);
}

#[test]
fn a_cancel_of_an_orphan_kills_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lock = store.placement_lock().unwrap();
    let mut r = laptop_job(&store, &lock, &[]);
    let mut sups = runs();
    let marker = CancelMarker {
        by: "w1".to_owned(),
        replaced_by: None,
    };
    sups.cancel(&store, &lock, &mut r, &marker).unwrap();
    assert_eq!(sups.processes.killed, Vec::<u32>::new());
    assert!(matches!(outcome_of(&store), Outcome::Cancelled { .. }));
}

#[test]
fn a_cancel_fails_on_a_garbled_pid_or_an_unwritable_history() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lock = store.placement_lock().unwrap();
    let mut r = laptop_job(&store, &lock, &[("pid", "not a pid")]);
    let held = store.job_lock("a").unwrap();
    let marker = CancelMarker {
        by: "w1".to_owned(),
        replaced_by: None,
    };
    runs().cancel(&store, &lock, &mut r, &marker).unwrap_err();
    drop(held);
    // The history is a directory: its write fails, as an I/O error.
    std::fs::create_dir_all(store.history()).unwrap();
    let err = runs().cancel(&store, &lock, &mut r, &marker).unwrap_err();
    assert!(err.to_string().contains("history"), "{err}");
}

#[test]
fn progress_reads_as_waiting_until_written() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(
        Progress::read(&store, "a", "l1").unwrap(),
        Progress {
            phase: Phase::Slot { ahead: 0 },
            elapsed_secs: 0,
            counters: None,
            started_at: None,
            left_secs: None,
            generated_per_min: None,
            queue_growth_permille: None,
        }
    );
    std::fs::create_dir_all(store.run_dir("a", "l1").join("scratch")).unwrap();
    std::fs::write(store.run_dir("a", "l1").join("scratch/x"), "").unwrap();
    clean(&store, "a", "l1").unwrap();
    clean(&store, "a", "l1").unwrap();
    assert!(!store.run_dir("a", "l1").join("scratch").exists());
}

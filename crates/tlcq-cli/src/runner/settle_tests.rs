//! Tests of settling a gone attempt.

use super::*;
use crate::runner::state::tests::{run, running};
use crate::runner::state::{Attempt, Slot};

/// The most one job gets on the VM, in MB: 110 GB.
const MOST: u64 = 112_640;

/// A state with run `v1` of `a` running its attempt 1 in `slot`.
fn running_at(slot: Slot, resume: bool) -> State {
    let mut s = State::default();
    let mut r = run("v1", running(), "A.cfg");
    r.last_attempt = 1;
    r.attempts.push(Attempt::new(1, slot, resume));
    s.runs.insert("a".to_owned(), vec![r]);
    s.queue.push(("a".to_owned(), "v1".to_owned()));
    s
}

fn started() -> Observed {
    Observed {
        started: true,
        duration_secs: 60,
        ..Observed::default()
    }
}

fn ended(s: &State) -> Option<&JobResult> {
    match &s.latest("a")?.phase {
        Phase::Ended { result } => Some(result),
        _ => None,
    }
}

#[test]
fn the_first_marker_in_order_gives_the_outcome() {
    let verdict = JobResult {
        duration_secs: 42,
        ..State::outcome_only(Outcome::Unexpected)
    };
    let cancel = Some(("ui".to_owned(), None));
    let cancelled = Outcome::Cancelled {
        by: "ui".to_owned(),
        replaced_by: None,
    };
    let error = |reason: &str| Outcome::Error {
        reason: Some(reason.to_owned()),
    };
    let cases = [
        (
            Observed {
                cancel: cancel.clone(),
                finished: Some(verdict.clone()),
                paused: true,
                kept: true,
                ..started()
            },
            cancelled.clone(),
        ),
        (
            Observed {
                finished: Some(verdict.clone()),
                paused: true,
                kept: true,
                timed_out: true,
                ..started()
            },
            Outcome::Unexpected,
        ),
        (
            Observed {
                no_checkpoint: true,
                timed_out: true,
                ..started()
            },
            cancelled,
        ),
        (
            Observed {
                timed_out: true,
                paused: true,
                ..started()
            },
            Outcome::Timeout,
        ),
        (
            Observed {
                paused: true,
                ..started()
            },
            error("stopped for a pause before its checkpoint was kept"),
        ),
        (started(), error("stopped: a shutdown or out of memory")),
    ];
    for (seen, outcome) in cases {
        let mut s = running_at(Slot::E, false);
        assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Ended);
        assert_eq!(ended(&s).map(|r| &r.outcome), Some(&outcome));
        assert_eq!(s.queue, Vec::new());
        assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Already);
    }
    let mut s = running_at(Slot::E, false);
    let seen = Observed {
        finished: Some(verdict.clone()),
        ..started()
    };
    s.settle("a", "v1", 1, &seen, (3, MOST));
    assert_eq!(ended(&s), Some(&verdict));
    assert!(s.cache.contains_key("a"));
}

#[test]
fn a_frozen_job_paused_before_its_first_checkpoint_ends_cancelled_and_says_so() {
    let mut s = running_at(Slot::P, false);
    let seen = Observed {
        no_checkpoint: true,
        ..started()
    };
    s.settle("a", "v1", 1, &seen, (3, MOST));
    assert_eq!(
        ended(&s).and_then(|r| r.note.as_deref()),
        Some("paused before its first checkpoint")
    );
}

#[test]
fn a_paused_stop_with_its_checkpoint_kept_pauses_the_run() {
    let mut s = running_at(Slot::P, false);
    let seen = Observed {
        paused: true,
        kept: true,
        ..started()
    };
    assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Paused);
    let r = s.latest("a").unwrap();
    assert!(r.paused() && r.attempts[0].ended && r.attempts[0].paused);
    assert!(matches!(&r.phase, Phase::Running { place, elapsed_secs: 60, .. } if place == "vm P"));
    assert_eq!(s.queue.len(), 1);
    assert!(s.cache.is_empty());
}

#[test]
fn an_attempt_that_never_started_is_undone_until_the_limit() {
    let mut s = running_at(Slot::E, false);
    s.runs.get_mut("a").unwrap()[0].undone = 1;
    assert_eq!(
        s.settle("a", "v1", 1, &Observed::default(), (3, MOST)),
        Settled::Undone
    );
    let r = s.latest("a").unwrap();
    assert_eq!(
        (r.attempts.len(), r.undone, &r.phase),
        (0, 2, &Phase::Queued)
    );
    let mut s = running_at(Slot::E, true);
    s.runs.get_mut("a").unwrap()[0].undone = 2;
    assert_eq!(
        s.settle("a", "v1", 1, &Observed::default(), (3, MOST)),
        Settled::Undone
    );
    assert_eq!(
        ended(&s).map(|r| &r.outcome),
        Some(&Outcome::Error {
            reason: Some("never started".to_owned())
        })
    );
    assert!(!s.latest("a").unwrap().resume);
}

#[test]
fn an_undone_resume_returns_to_the_head() {
    let mut s = running_at(Slot::E, true);
    assert_eq!(
        s.settle("a", "v1", 1, &Observed::default(), (3, MOST)),
        Settled::Undone
    );
    let r = s.latest("a").unwrap();
    assert!(r.resume && r.attempts.is_empty());
    assert!(matches!(r.phase, Phase::Running { .. }));
}

#[test]
fn a_cancel_before_the_start_ends_the_run_and_a_start_resets_the_count() {
    let mut s = running_at(Slot::E, false);
    s.runs.get_mut("a").unwrap()[0].undone = 2;
    let seen = Observed {
        cancel: Some(("agent".to_owned(), Some("b".to_owned()))),
        ..Observed::default()
    };
    assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Ended);
    assert_eq!(s.latest("a").unwrap().undone, 2);
    let mut s = running_at(Slot::E, false);
    s.runs.get_mut("a").unwrap()[0].undone = 2;
    s.settle("a", "v1", 1, &started(), (3, MOST));
    assert_eq!(s.latest("a").unwrap().undone, 0);
    assert_eq!(
        s.settle("b", "v1", 1, &started(), (3, MOST)),
        Settled::Already
    );
}

#[test]
fn a_cancelled_paused_run_keeps_the_run_s_duration_and_lanes() {
    use crate::outcome::{Lane, LaneDecision};
    use crate::runner::store::CancelMarker;
    let mut s = running_at(Slot::E, false);
    let max = LaneDecision {
        to: Lane::Max,
        after_secs: 900,
        probe: None,
    };
    let seen = Observed {
        paused: true,
        kept: true,
        lanes: vec![max.clone()],
        duration_secs: 905,
        ..started()
    };
    assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Paused);
    let marker = CancelMarker {
        by: "agent".to_owned(),
        replaced_by: None,
    };
    assert_eq!(s.cancel_run("a", "v1", &marker), None);
    let result = ended(&s).unwrap();
    assert_eq!(
        (result.duration_secs, result.lanes.clone()),
        (905, vec![max])
    );
    assert!(result.judged_max());
}

#[test]
fn a_marker_s_end_keeps_tlc_s_last_counts_and_the_slot() {
    let mut s = running_at(Slot::P, false);
    let seen = Observed {
        timed_out: true,
        counts: Some(Counts {
            depth: 12,
            generated: 900,
            distinct: 300,
            queue: 40,
        }),
        ..started()
    };
    assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Ended);
    let r = ended(&s).unwrap();
    assert_eq!(
        (
            &r.outcome,
            r.distinct,
            r.generated,
            r.depth,
            r.place.as_str()
        ),
        (&Outcome::Timeout, Some(300), Some(900), Some(12), "vm P")
    );
}

#[test]
fn a_resume_that_died_pauses_the_run_again_on_its_checkpoint() {
    let mut s = running_at(Slot::E, true);
    let seen = Observed {
        recovered: true,
        signal: Some(9),
        ..started()
    };
    assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Paused);
    let r = s.latest("a").unwrap();
    assert!(r.paused());
    assert_eq!(
        r.attempts[0].died.as_deref(),
        Some("stopped: killed (signal 9), most likely out of memory")
    );
    // A marker or a verdict still ranks first.
    let mut s = running_at(Slot::E, true);
    let seen = Observed {
        timed_out: true,
        ..seen
    };
    assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Ended);
}

#[test]
fn a_resume_its_engine_refused_before_recovering_pauses_the_run_again() {
    let refused = State::outcome_only(Outcome::Error {
        reason: Some("cannot recover from /c: a different tlx build".to_owned()),
    });
    let mut s = running_at(Slot::E, true);
    let seen = Observed {
        recovered: true,
        finished: Some(refused),
        ..started()
    };
    assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Paused);
    let r = s.latest("a").unwrap();
    assert!(r.paused());
    assert_eq!(
        r.attempts[0].died.as_deref(),
        Some("cannot recover from /c: a different tlx build")
    );
    // Any other exit before the recovery says so in general terms.
    let mut s = running_at(Slot::E, true);
    let other = Observed {
        finished: Some(State::outcome_only(Outcome::Unexpected)),
        ..seen.clone()
    };
    assert_eq!(s.settle("a", "v1", 1, &other, (3, MOST)), Settled::Paused);
    assert_eq!(
        s.latest("a").unwrap().attempts[0].died.as_deref(),
        Some("its engine exited before recovering the checkpoint")
    );
    // Once the recovery completed, the engine's verdict ends the run.
    let mut s = running_at(Slot::E, true);
    let seen = Observed {
        recovery_completed: true,
        ..seen
    };
    assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Ended);
}

#[test]
fn a_long_run_whose_supervisor_kept_its_checkpoint_as_it_died_is_paused() {
    // RS_pack_zombie2: 7h in, the kernel killed tlx for memory.
    let mut s = running_at(Slot::E, false);
    s.runs.get_mut("a").unwrap()[0].attempts[0].memory_mb = Some(54_380);
    let seen = Observed {
        kept: true,
        signal: Some(9),
        ..started()
    };
    assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Paused);
    let r = s.latest("a").unwrap();
    assert!(r.paused());
    assert_eq!(
        r.attempts[0].died.as_deref(),
        Some("stopped: killed (signal 9), most likely out of memory; needs more memory than its 54380 MB: ask for up to --memory 110G, the most one job gets on the VM")
    );
    // An engine's error, with its note.
    let mut s = running_at(Slot::E, false);
    let error = JobResult {
        note: Some("needs more memory: resubmit with --memory 59G".to_owned()),
        ..State::outcome_only(Outcome::Error {
            reason: Some("out of memory".to_owned()),
        })
    };
    let seen = Observed {
        kept: true,
        finished: Some(error),
        ..started()
    };
    assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Paused);
    assert_eq!(
        s.latest("a").unwrap().attempts[0].died.as_deref(),
        Some("out of memory; needs more memory: resubmit with --memory 59G")
    );
    let mut s = running_at(Slot::E, false);
    let seen = Observed {
        finished: Some(State::outcome_only(Outcome::Error { reason: None })),
        ..seen
    };
    s.settle("a", "v1", 1, &seen, (3, MOST));
    assert_eq!(
        s.latest("a").unwrap().attempts[0].died.as_deref(),
        Some("error")
    );
    // A verdict past an error ends it, as does a cancel.
    let mut s = running_at(Slot::E, false);
    let seen = Observed {
        finished: Some(State::outcome_only(Outcome::Unexpected)),
        ..seen
    };
    assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Ended);
    let mut s = running_at(Slot::E, false);
    let seen = Observed {
        finished: None,
        cancel: Some(("ui".to_owned(), None)),
        ..seen
    };
    assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Ended);
}

#[test]
fn a_run_killed_for_memory_says_what_it_had() {
    let mut s = running_at(Slot::E, false);
    s.runs.get_mut("a").unwrap()[0].attempts[0].memory_mb = Some(54_380);
    let seen = Observed {
        signal: Some(9),
        ..started()
    };
    s.settle("a", "v1", 1, &seen, (3, MOST));
    assert_eq!(
        ended(&s).and_then(|r| r.note.as_deref()),
        Some("needs more memory than its 54380 MB: ask for up to --memory 110G, the most one job gets on the VM")
    );
    // Killed by another signal: no hint.
    let mut s = running_at(Slot::E, false);
    s.runs.get_mut("a").unwrap()[0].attempts[0].memory_mb = Some(54_380);
    let seen = Observed {
        signal: Some(11),
        ..started()
    };
    s.settle("a", "v1", 1, &seen, (3, MOST));
    assert_eq!(ended(&s).and_then(|r| r.note.as_deref()), None);
}

#[test]
fn an_engine_that_died_says_how() {
    for (signal, out_of_memory, reason) in [
        (None, false, "stopped: a shutdown or out of memory"),
        (
            Some(9),
            false,
            "stopped: killed (signal 9), most likely out of memory",
        ),
        (Some(11), false, "stopped: killed by signal 11"),
        (Some(9), true, "stopped: out of memory"),
    ] {
        let mut s = running_at(Slot::E, false);
        let seen = Observed {
            signal,
            out_of_memory,
            ..started()
        };
        s.settle("a", "v1", 1, &seen, (3, MOST));
        assert_eq!(
            ended(&s).map(|r| &r.outcome),
            Some(&Outcome::Error {
                reason: Some(reason.to_owned())
            })
        );
    }
}

#[test]
fn a_long_run_stopped_at_its_cap_with_its_checkpoint_kept_is_paused() {
    let mut s = running_at(Slot::E, false);
    let seen = Observed {
        kept: true,
        timed_out: true,
        ..started()
    };
    assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Paused);
    assert_eq!(
        s.latest("a").unwrap().attempts[0].died.as_deref(),
        Some("stopped at its time cap: resume it with a longer --timeout")
    );
    // With none kept, it times out.
    let mut s = running_at(Slot::E, false);
    let seen = Observed {
        kept: false,
        ..seen
    };
    assert_eq!(s.settle("a", "v1", 1, &seen, (3, MOST)), Settled::Ended);
    assert!(matches!(
        ended(&s).map(|r| &r.outcome),
        Some(Outcome::Timeout)
    ));
}

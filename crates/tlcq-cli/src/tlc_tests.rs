//! Tests of reading TLC, against logs of a real project's runs.

use super::*;

const PASS: &str = include_str!("../tests/fixtures/tlc/pass.log");
const INVARIANT: &str = include_str!("../tests/fixtures/tlc/invariant.log");
const TEMPORAL: &str = include_str!("../tests/fixtures/tlc/temporal.log");
const POSSIBLE: &str = include_str!("../tests/fixtures/tlc/possible.log");
const DEADLOCK: &str = include_str!("../tests/fixtures/tlc/deadlock.log");
const CONFIG_ERROR: &str = include_str!("../tests/fixtures/tlc/config-error.log");
const PROGRESS: &str = include_str!("../tests/fixtures/tlc/progress.log");
const STOPPED: &str = include_str!("../tests/fixtures/tlc/stopped.log");
const PARSE_ERROR: &str = include_str!("../tests/fixtures/tlc/parse-error.log");
const EXIT_STATUS: &str = include_str!("../tests/fixtures/tlc/exit-status.txt");

/// The exit status TLC gave the fixture `name`, as measured.
fn exit(name: &str) -> Option<i32> {
    EXIT_STATUS
        .lines()
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix(name)?.strip_prefix(' ')?.parse().ok())
}

fn must(p: &str) -> MustFail {
    MustFail::Property(p.to_owned())
}

#[test]
fn a_progress_line_s_time_is_read_across_days_months_and_years() {
    let at = |stamp: &str| {
        progress_time(&format!(
            "Progress(27) at {stamp}: 1,375,594,240 states generated (9,497,670 s/min), \
             362,435,524 distinct states found (1,214,776 ds/min), 81,647,703 states left on queue."
        ))
    };
    assert_eq!(at("1970-01-01 00:00:00"), Some(0));
    let t = at("2026-10-19 09:22:44").unwrap();
    assert_eq!(at("2026-10-19 09:23:44"), Some(t + 60));
    assert_eq!(
        at("2026-10-20 00:00:00"),
        Some(t + 14 * 3600 + 37 * 60 + 16)
    );
    assert_eq!(
        at("2025-03-01 00:00:00").unwrap() - at("2025-02-28 00:00:00").unwrap(),
        86_400
    );
    assert_eq!(
        at("2025-01-01 00:00:00").unwrap() - at("2024-12-31 00:00:00").unwrap(),
        86_400
    );
    assert_eq!(at("2026-10-19 09:2"), None);
    assert_eq!(at("2026-1x-19 09:22:44"), None);
    assert_eq!(
        progress_time("Checkpointing completed at (2026-10-18 18:17:45)"),
        None
    );
}

#[test]
fn a_checkpoint_and_the_fingerprint_function_are_read_from_the_log() {
    assert!(checkpointed(
        "Checkpointing completed at (2026-10-18 18:17:45)"
    ));
    assert!(!checkpointed(
        "Checkpointing of run /s/26-10-18-16-47-35.165"
    ));
    assert_eq!(fingerprint_fn(PASS), Some(8));
    assert_eq!(fingerprint_fn("Running with fp x"), None);
    assert_eq!(
        fingerprint_fn(CONFIG_ERROR.lines().next().unwrap_or("")),
        None
    );
    let done = "Recovery completed. 199106854 states examined. 60935130 states on queue.";
    assert_eq!(recovered(done), Some(199_106_854));
    assert_eq!(recovered("Recovery completed. 12 states left."), None);
    assert_eq!(recovered("Starting recovery from checkpoint /c/tlc/"), None);
}

#[test]
fn progress_lines_are_read_with_or_without_rates() {
    let with = "Progress(17) at 2026-10-07 17:15:51: 11,287,793 states generated (10,914,495 s/min), \
                3,359,640 distinct states found (3,222,692 ds/min), 1,791,313 states left on queue.";
    let counts = Counts {
        depth: 17,
        generated: 11_287_793,
        distinct: 3_359_640,
        queue: 1_791_313,
    };
    assert_eq!(progress(with), Some(counts));
    let without = "Progress(3) at 2026-10-07 17:15:51: 1,200 states generated, 900 distinct states found, 40 states left on queue.";
    assert_eq!(progress(without).map(|c| (c.depth, c.queue)), Some((3, 40)));
    assert_eq!(
        progress("Progress(3) at x: 12 states generated, 9 apples, 1 states left on queue."),
        None
    );
    assert_eq!(progress("Starting... (2026-10-03 16:15:15)"), None);
}

#[test]
fn a_log_gives_its_final_counts_and_depth() {
    let pass = summary(PASS);
    let counts = Counts {
        depth: 10,
        generated: 51_804,
        distinct: 21_595,
        queue: 0,
    };
    assert_eq!(pass.counts, Some(counts));
    assert!(pass.errors.is_empty() && !pass.out_of_memory);
    let stopped = summary(PROGRESS);
    assert_eq!(
        stopped.counts.map(|c| (c.depth, c.distinct)),
        Some((18, 14_788_081))
    );
    assert_eq!(summary(STOPPED).counts.map(|c| c.depth), Some(12));
    assert!(
        summary("Exception in thread main java.lang.OutOfMemoryError: Java heap space")
            .out_of_memory
    );
}

/// The exit statuses are TLC 2026.09.25's, measured on the pinned jar and kept
/// beside the fixtures (`exit-status.txt`).
#[test]
fn a_run_passes_or_fails_as_its_header_expects() {
    assert_eq!(
        [
            "pass",
            "deadlock",
            "invariant",
            "temporal",
            "possible",
            "parse-error"
        ]
        .map(exit),
        [Some(0), Some(11), Some(12), Some(13), Some(10), Some(150)]
    );
    assert_eq!(verdict(Some(0), None, &summary(PASS)), Outcome::Ok);
    assert_eq!(
        verdict(Some(12), None, &summary(INVARIANT)),
        Outcome::Unexpected
    );
    let invariant = summary(INVARIANT);
    assert_eq!(
        verdict(Some(12), Some(&must("SafetyA")), &invariant),
        Outcome::Ok
    );
    assert_eq!(
        verdict(Some(12), Some(&must("Dropped")), &invariant),
        Outcome::Unexpected
    );
    assert_eq!(
        verdict(Some(0), Some(&must("SafetyA")), &summary(PASS)),
        Outcome::Unexpected
    );
    let temporal = summary(TEMPORAL);
    assert_eq!(
        verdict(Some(13), Some(&must("LivenessA")), &temporal),
        Outcome::Ok
    );
    assert_eq!(
        verdict(Some(13), Some(&must("LivenessB")), &temporal),
        Outcome::Ok
    );
    assert_eq!(
        verdict(Some(10), Some(&must("PossibleA")), &summary(POSSIBLE)),
        Outcome::Ok
    );
    let deadlock = summary(DEADLOCK);
    assert_eq!(
        verdict(Some(11), Some(&MustFail::Deadlock), &deadlock),
        Outcome::Ok
    );
    assert_eq!(
        verdict(Some(11), Some(&MustFail::Deadlock), &invariant),
        Outcome::Unexpected
    );
}

#[test]
fn errors_tlc_reports_itself_are_unexpected_and_the_rest_error() {
    let config = summary(CONFIG_ERROR);
    assert_eq!(config.errors.len(), 1);
    assert_eq!(verdict(Some(150), None, &config), Outcome::Unexpected);
    assert_eq!(verdict(Some(75), None, &config), Outcome::Unexpected);
    // A spec that does not parse: deterministic, so `unexpected` and cached.
    let parse = summary(PARSE_ERROR);
    assert!(parse
        .errors
        .iter()
        .any(|e| e.contains("Parsing or semantic analysis failed")));
    assert_eq!(
        verdict(exit("parse-error"), None, &parse),
        Outcome::Unexpected
    );
    let reason = |o: Outcome| match o {
        Outcome::Error { reason } => reason,
        other => panic!("not an error: {other:?}"),
    };
    assert_eq!(
        reason(verdict(Some(255), None, &config)).as_deref(),
        Some("TLC exited 255")
    );
    assert_eq!(
        reason(verdict(None, None, &summary(STOPPED))).as_deref(),
        Some("TLC did not exit")
    );
    let oom = Summary {
        out_of_memory: true,
        ..Summary::default()
    };
    assert_eq!(
        reason(verdict(Some(255), None, &oom)).as_deref(),
        Some("out of memory")
    );
}

#[test]
fn another_violation_first_names_it_and_suggests_must_fail_only() {
    let invariant = summary(INVARIANT);
    let other = must("SafetyB");
    let outcome = verdict(Some(12), Some(&other), &invariant);
    assert_eq!(outcome, Outcome::Unexpected);
    let named = note(&outcome, Some(&other), &invariant).unwrap();
    assert_eq!(
        named,
        "TLC reported \"Invariant SafetyA is violated.\", not SafetyB; --must-fail-only checks only SafetyB"
    );
    let deadlock = note(&Outcome::Unexpected, Some(&MustFail::Deadlock), &invariant).unwrap();
    assert!(deadlock.ends_with("checks only a deadlock"), "{deadlock}");
    assert_eq!(note(&Outcome::Ok, Some(&other), &invariant), None);
    assert_eq!(note(&Outcome::Unexpected, None, &invariant), None);
    let deadlocked = note(&Outcome::Unexpected, Some(&other), &summary(DEADLOCK)).unwrap();
    assert!(
        deadlocked.starts_with("TLC reported \"Deadlock reached."),
        "{deadlocked}"
    );
    let unwitnessed = note(&Outcome::Unexpected, Some(&other), &summary(POSSIBLE)).unwrap();
    assert!(unwitnessed.contains("was never witnessed"), "{unwitnessed}");
    assert_eq!(
        note(&Outcome::Unexpected, Some(&other), &summary(PASS)),
        None
    );
}

mod tlx {
    //! tlx's logs, read with TLC's rules (tlx-engine E5).

    use super::*;

    const PASS: &str = include_str!("../tests/fixtures/tlx/pass.log");
    const LIVE_PASS: &str = include_str!("../tests/fixtures/tlx/live-pass.log");
    /// `pass.log` with the report `-stats` adds as tlx exits (from another model).
    const PASS_STATS: &str = include_str!("../tests/fixtures/tlx/pass-stats.log");
    const INVARIANT: &str = include_str!("../tests/fixtures/tlx/invariant.log");
    const TEMPORAL: &str = include_str!("../tests/fixtures/tlx/temporal.log");
    const EXIT_STATUS: &str = include_str!("../tests/fixtures/tlx/exit-status.txt");

    /// The exit status tlx gave the fixture `name`, as measured.
    fn exit(name: &str) -> Option<i32> {
        EXIT_STATUS
            .lines()
            .filter(|l| !l.starts_with('#'))
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(' ')?.parse().ok())
    }

    #[test]
    fn a_passing_tlx_run_gives_tlc_s_counts_and_verdict() {
        for (name, log) in [("pass", PASS), ("live-pass", LIVE_PASS)] {
            let s = summary(log);
            assert!(s.tlx, "{name}");
            let c = s.counts.unwrap();
            assert_eq!(
                (c.generated, c.distinct, c.queue, c.depth),
                (51_804, 21_595, 0, 10),
                "{name}"
            );
            assert_eq!(verdict(exit(name), None, &s), Outcome::Ok, "{name}");
        }
        // The report changes neither the counts nor the verdict.
        let s = summary(PASS_STATS);
        assert_eq!(
            (s.counts, verdict(exit("pass"), None, &s)),
            (summary(PASS).counts, Outcome::Ok)
        );
        assert_eq!(fingerprint_fn(PASS), Some(0));
        assert!(!summary(super::PASS).tlx);
    }

    #[test]
    fn a_tlx_violation_names_the_property_as_tlc_does() {
        let s = summary(INVARIANT);
        assert_eq!(s.errors[0], "Error: Invariant SafetyB is violated.");
        assert_eq!(
            verdict(exit("invariant"), Some(&must("SafetyB")), &s),
            Outcome::Ok
        );
        let t = summary(TEMPORAL);
        assert_eq!(
            t.counts.map(|c| (c.distinct, c.depth)),
            Some((1_420_685, 27))
        );
        assert_eq!(
            verdict(exit("temporal"), Some(&must("LivenessC")), &t),
            Outcome::Ok
        );
        let other = verdict(exit("temporal"), Some(&must("SafetyB")), &t);
        assert_eq!(other, Outcome::Unexpected);
        let n = note(&other, Some(&must("SafetyB")), &t).unwrap();
        assert!(
            n.starts_with("tlx reported \"Temporal property LivenessC"),
            "{n}"
        );
        assert_eq!(refusal(exit("temporal"), &t), None);
    }

    #[test]
    fn a_tlx_error_says_why_in_tlx_s_words() {
        let log = |errors: &str| summary(&format!("tlx version 0.1.0\n{errors}"));
        let reason = |exit, s: &Summary| match verdict(exit, None, s) {
            Outcome::Error { reason } => reason.unwrap(),
            o => panic!("{o:?}"),
        };
        assert_eq!(reason(Some(101), &log("")), "tlx panicked");
        let liveness =
            log("Error: liveness needs more memory: 17000 MiB over the 15600 MiB budget\n");
        assert_eq!(
            reason(Some(255), &liveness),
            "liveness needs more memory: 17000 MiB over the 15600 MiB budget"
        );
        assert_eq!(reason(Some(255), &log("")), "tlx exited 255");
        assert_eq!(reason(Some(3), &log("")), "tlx exited 3");
        assert_eq!(reason(None, &log("")), "tlx did not exit");
        // TLC's own words are kept for TLC.
        assert_eq!(reason(Some(255), &summary("Error: x\n")), "TLC exited 255");
        assert_eq!(reason(Some(101), &summary("")), "TLC exited 101");
    }

    #[test]
    fn a_run_tlx_refuses_carries_the_hint_to_try_tlc() {
        let parse = summary("tlx version 0.1.0\nError: Parsing or semantic analysis failed.\n");
        let hint = refusal(Some(150), &parse).unwrap();
        assert_eq!(
            hint,
            "tlx refused (exit 150): Parsing or semantic analysis failed.; resubmit with --engine tlc if TLC accepts it"
        );
        assert_eq!(verdict(Some(150), None, &parse), Outcome::Unexpected);
        assert!(refusal(Some(2), &summary("tlx version 0.1.0\n"))
            .unwrap()
            .contains("no Error: line"));
        assert!(refusal(Some(101), &parse).is_some());
        assert_eq!(refusal(Some(12), &parse), None);
        assert_eq!(refusal(None, &parse), None);
        // TLC refusing a spec is the spec's fault: no hint.
        assert_eq!(refusal(Some(150), &summary("Error: x\n")), None);
    }
}

#[test]
fn tlx_s_queue_of_one_level_is_counted_as_tlc_counts_it() {
    let c = |depth, distinct, queue| Counts {
        depth,
        generated: distinct * 8,
        distinct,
        queue,
    };
    // RS_pack_zombie2's levels 30 and 31: the queue drains to nothing,
    // then jumps by the whole next level at the boundary.
    let lines = [
        c(30, 1_535_000_000, 700_000_000),
        c(30, 2_237_168_475, 12_421_757),
        c(30, 2_243_771_316, 908_628),
        c(31, 2_243_889_334, 708_789_502),
        c(31, 2_250_000_000, 690_000_000),
    ];
    let mut tlx = Pending::default();
    let first: Vec<u64> = lines.iter().map(|&l| tlx.add(l).queue).collect();
    // Until the jump shows it, as it is; from the boundary on, with the
    // states found since the level started.
    assert_eq!(first[..3], [700_000_000, 12_421_757, 908_628]);
    assert!(tlx.counts_levels());
    assert_eq!(first[3..], [708_907_520, 696_228_684]);
    // Known from the start: smooth across the boundary.
    let mut known = Pending::levels();
    let all: Vec<u64> = lines.iter().map(|&l| known.add(l).queue).collect();
    assert_eq!(
        all,
        [
            700_000_000,
            714_590_232,
            709_679_944,
            708_907_520,
            696_228_684
        ]
    );
    // TLC's queue grows by no more than the states found: as it is.
    let mut tlc = Pending::default();
    for l in [c(5, 100, 40), c(6, 150, 70), c(7, 200, 60)] {
        assert_eq!(tlc.add(l), l);
    }
    assert!(!tlc.counts_levels());
}

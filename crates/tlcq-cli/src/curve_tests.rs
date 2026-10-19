//! Tests of the progress curve: its TSV, and its recording from TLC's logs.

use super::*;

const TEMPORAL: &str = include_str!("../tests/fixtures/tlc/temporal.log");
const PROGRESS: &str = include_str!("../tests/fixtures/tlc/progress.log");
const PASS: &str = include_str!("../tests/fixtures/tlc/pass.log");

/// Record `log`, a line a second, and read the curve back.
fn record(log: &str) -> Curve {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE);
    let mut r = Recorder::new(path.clone()).unwrap();
    let mut secs = 0;
    for line in log.lines() {
        secs += 1;
        r.line(secs, line).unwrap();
    }
    r.end(secs, tlc::summary(log).counts).unwrap();
    Curve::read(&path).unwrap().unwrap()
}

#[test]
fn a_curve_round_trips_through_its_tsv() {
    let c = Curve {
        points: vec![Point {
            secs: 63.0,
            depth: 14,
            generated: 9_234_007.0,
            distinct: 3_379_510.0,
            queue: 1_456_153.0,
        }],
        temporal: true,
        check_secs: Some(1100.0),
        complete: true,
        last: Some((9_811_782.0, 3_000_000.0)),
        end_secs: Some(1234.0),
        left: Vec::new(),
    };
    assert_eq!(Curve::parse(&c.to_jsonl()), c);
    let cut = Curve::parse("# tlcq curve\n1\t2\t3\t4\t5\n6\t7\t8\n# check\n# what 1\n");
    assert_eq!((cut.points.len(), cut.check_secs), (1, None));
    let none = tempfile::tempdir().unwrap();
    assert_eq!(Curve::read(&none.path().join("x.tsv")).unwrap(), None);
    Curve::read(none.path()).unwrap_err();
}

#[test]
fn a_recording_keeps_each_progress_line_and_what_the_log_says_of_the_end() {
    let progress = record(PROGRESS);
    assert_eq!(progress.points.len(), 6);
    assert_eq!(progress.points[1].distinct, 3_562_718.0);
    assert!(!progress.temporal && !progress.complete && progress.end_secs.is_some());
    let pass = record(PASS);
    assert!(pass.complete);
    assert_eq!(pass.last, Some((51_804.0, 21_595.0)));
    let temporal = record(TEMPORAL);
    assert!(temporal.temporal);
    // Its checks are of the current state space, not the final one.
    assert_eq!(temporal.check_secs, None);
    let final_check = "Checking 6 branches of temporal properties for the complete state space with 9 total distinct states at (2026-10-03 15:45:32)";
    let checked = record(&format!("{final_check}\n{final_check}\n"));
    assert_eq!(checked.check_secs, Some(1.0));
}

#[test]
fn a_progress_line_is_recorded_at_tlc_s_own_spacing_unless_a_sleep_came_between() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE);
    let mut r = Recorder::new(path.clone()).unwrap();
    let line = |stamp: &str| {
        format!(
            "Progress(9) at 2026-10-19 {stamp}: 3,000 states generated (1 s/min), \
             1,000 distinct states found (1 ds/min), 200 states left on queue."
        )
    };
    // Seen 63, 118 and 181 seconds in: TLC wrote them a minute apart.
    for (seen, stamp) in [(63, "09:21:44"), (118, "09:22:44"), (181, "09:23:44")] {
        r.line(seen, &line(stamp)).unwrap();
    }
    // A laptop sleep of an hour: TLC's clock went on, awake time did not.
    r.line(245, &line("10:24:44")).unwrap();
    // A line TLC wrote earlier than the one before (its clock moved back).
    r.line(305, &line("10:20:44")).unwrap();
    let secs: Vec<f64> = Curve::read(&path)
        .unwrap()
        .unwrap()
        .points
        .iter()
        .map(|p| p.secs)
        .collect();
    assert_eq!(secs, [63.0, 123.0, 183.0, 245.0, 305.0]);
}

#[test]
fn an_attempt_s_curve_is_its_jsonl_else_an_older_attempt_s_tsv() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(text_in(dir.path()).unwrap(), None);
    std::fs::write(
        dir.path().join(LEGACY_FILE),
        "# tlcq curve\n60\t3\t9\t5\t2\n",
    )
    .unwrap();
    assert_eq!(path_in(dir.path()), dir.path().join(LEGACY_FILE));
    assert_eq!(Curve::read_in(dir.path()).unwrap().unwrap().points.len(), 1);
    std::fs::write(dir.path().join(FILE), "{\"kind\":\"complete\"}\n").unwrap();
    assert!(Curve::read_in(dir.path()).unwrap().unwrap().complete);
    // One that cannot be read is an error, not no curve.
    std::fs::remove_file(dir.path().join(FILE)).unwrap();
    std::fs::create_dir(dir.path().join(FILE)).unwrap();
    text_in(dir.path()).unwrap_err();
}

#[test]
fn a_recording_into_a_missing_directory_fails() {
    let dir = tempfile::tempdir().unwrap();
    Recorder::new(dir.path().join("no").join(FILE)).unwrap_err();
    let path = dir.path().join(FILE);
    let mut r = Recorder::new(path.clone()).unwrap();
    std::fs::remove_file(&path).unwrap();
    r.line(1, "Model checking completed. No error has been found.")
        .unwrap_err();
}

#[test]
fn rates_need_two_lines_apart_and_distinct_states() {
    let mut c = Curve::parse("# tlcq curve\n60\t3\t1000\t500\t100\n");
    assert_eq!(c.rates(300.0), None);
    c.points.push(Point {
        secs: 420.0,
        depth: 4,
        generated: 7000.0,
        distinct: 2000.0,
        queue: 300.0,
    });
    let r = c.rates(300.0).unwrap();
    assert!((r.generated_per_min - 1000.0).abs() < 1e-9 && (r.queue_growth - 0.1).abs() < 1e-9);
    // Only the last line falls in the window: no slope to fit.
    assert_eq!(r.queue_per_min, None);
    assert_eq!(Curve::default().rates(300.0), None);
}

/// A curve with a progress line each minute, its queue `queue(minute)`.
fn minutes(n: u32, queue: impl Fn(f64) -> f64) -> Curve {
    use std::fmt::Write;
    let mut text = String::from("# tlcq curve\n");
    for m in 0..=n {
        let t = f64::from(m);
        let line = (t * 60.0, 100.0 * t + 1.0, 50.0 * t + 1.0, queue(t));
        writeln!(text, "{}\t1\t{}\t{}\t{}", line.0, line.1, line.2, line.3).unwrap();
    }
    Curve::parse(&text)
}

#[test]
fn the_queue_s_slope_is_fitted_over_a_sliding_window_and_kept_as_a_history() {
    // A steady 1000 a minute: that slope, the same all along.
    let steady = minutes(40, |t| 1000.0 * t);
    let slope = steady.rates(300.0).unwrap().queue_per_min;
    assert!(
        slope.is_some_and(|s| (s - 1000.0).abs() < 1e-6),
        "{slope:?}"
    );
    let history = steady.queue_slopes(300.0, 1800.0);
    // A line a minute over the last 30 minutes, each with 5 minutes behind it.
    assert_eq!(history.len(), 31);
    assert!(
        history.iter().all(|s| (s - 1000.0).abs() < 1e-6),
        "{history:?}"
    );
    // Speeding up (queue 20t²): each slope steeper than the one before.
    let faster = minutes(40, |t| 20.0 * t * t).queue_slopes(300.0, 1800.0);
    assert!(faster.windows(2).all(|w| w[1] > w[0]), "{faster:?}");
    assert!((faster.last().unwrap() - 1500.0).abs() < 1e-6, "{faster:?}");
    // Draining: negative, ever steeper.
    let draining = minutes(40, |t| 100_000.0 - 20.0 * t * t).queue_slopes(300.0, 1800.0);
    assert!(
        draining.windows(2).all(|w| w[1] < w[0] && w[1] < 0.0),
        "{draining:?}"
    );
    // Shorter than a window: each line but the first, fitted over the lines so far.
    let young = minutes(4, |t| 60.0 * t).queue_slopes(300.0, 1800.0);
    assert_eq!(young.len(), 4);
    assert!(young.iter().all(|s| (s - 60.0).abs() < 1e-6), "{young:?}");
    assert_eq!(
        Curve::default().queue_slopes(300.0, 1800.0),
        Vec::<f64>::new()
    );
}

#[test]
fn a_tlx_recording_marks_the_temporal_check_and_the_end_as_tlc_s_does() {
    let live = record(include_str!("../tests/fixtures/tlx/live-pass.log"));
    assert!(live.temporal && live.complete);
    assert!(live.check_secs.is_some());
    assert_eq!(live.last, Some((51_804.0, 21_595.0)));
    let temporal = record(include_str!("../tests/fixtures/tlx/temporal.log"));
    assert_eq!(temporal.points.len(), 2);
    assert_eq!(temporal.points[1].distinct, 1_420_685.0);
    assert!(temporal.temporal && temporal.check_secs.is_some() && !temporal.complete);
}

#[test]
fn a_tlx_curve_reads_its_queue_as_tlc_counts_it_and_a_tlc_curve_as_it_is() {
    let line = |secs, depth, distinct, queue| {
        format!(
            r#"{{"kind":"progress","secs":{secs},"depth":{depth},"generated":0,"distinct":{distinct},"queue":{queue}}}"#
        )
    };
    // tlx: the queue drains with level 30, then jumps by level 31.
    let tlx = [
        line(0, 29, 100, 10),
        line(60, 30, 120, 50),
        line(120, 30, 160, 2),
        line(180, 31, 161, 60),
    ]
    .join("\n");
    let queues = |text: &str| {
        Curve::parse(text)
            .points
            .iter()
            .map(|p| p.queue)
            .collect::<Vec<_>>()
    };
    assert_eq!(queues(&tlx), [10.0, 70.0, 62.0, 61.0]);
    assert_eq!(Curve::parse(&tlx).counters().map(|c| c.queue), Some(61));
    // RS_pack_zombie2_one's level 24: its first line was taken while the
    // level was still being queued (33M, then 48M): it counts the later one.
    let partial = [
        line(0, 23, 127_047_427, 3_516_561),
        line(60, 24, 130_670_749, 32_983_772),
        line(120, 24, 137_353_841, 47_909_607),
        line(180, 24, 144_191_025, 43_832_910),
    ]
    .join("\n");
    assert_eq!(
        queues(&partial)[1..],
        [51_532_929.0, 58_216_021.0, 60_976_508.0]
    );
    // What is left of level 24 drains while the queue grows.
    let level = Curve::parse(&partial).level(300.0).unwrap();
    assert_eq!((level.depth, level.left), (24, 43_832_910.0));
    assert!(level.per_min.unwrap() < 0.0, "{level:?}");
    assert_eq!(Curve::parse(&partial).left[1], 47_909_607.0);
    // TLC's queue never grows by more than the states found: as it is.
    let tlc = [
        line(0, 29, 100, 10),
        line(60, 30, 120, 25),
        line(120, 31, 160, 40),
    ]
    .join("\n");
    assert_eq!(queues(&tlc), [10.0, 25.0, 40.0]);
    assert_eq!(Curve::parse(&tlc).level(300.0), None);
}

#[test]
fn a_search_s_depth_comes_from_its_last_complete_run_or_its_shrinking_levels() {
    // One line per level, at its end: the distinct states so far.
    let curve = |ends: &[(u64, u64)]| {
        let lines: Vec<String> = ends
            .iter()
            .enumerate()
            .map(|(i, (depth, distinct))| {
                format!(
                    r#"{{"kind":"progress","secs":{},"depth":{depth},"generated":0,"distinct":{distinct},"queue":0}}"#,
                    i * 60
                )
            })
            .collect();
        Curve::parse(&lines.join("\n"))
    };
    let depth = |c: &Curve| c.depth(None).map(|d| (d.now, d.total, d.from));
    // Growing: how deep it goes is not known yet.
    let growing = curve(&[(1, 10), (2, 30), (3, 70), (4, 150), (5, 300)]);
    assert_eq!(depth(&growing), Some((5, None, None)));
    // Levels 4, 5, 6 of 1000, 100, 10 states: level 7, of about one, ends it.
    let shrinking = curve(&[
        (1, 1000),
        (2, 3000),
        (3, 4000),
        (4, 4100),
        (5, 4110),
        (6, 4111),
    ]);
    assert_eq!(
        shrinking.depth(None).map(|d| (d.now, d.total, d.from)),
        Some((6, Some(7), Some(DepthSource::Shrinking)))
    );
    let shrinking = curve(&[(1, 1000), (2, 3000), (3, 4000), (4, 4100), (5, 4110)]);
    // A level earlier (level 5 of 100): two more.
    assert_eq!(
        depth(&shrinking),
        Some((5, Some(7), Some(DepthSource::Shrinking)))
    );
    // A complete run of the configuration ended at depth 9.
    let previous = curve(&[(1, 10), (9, 90)]);
    assert_eq!(
        growing.depth(Some(&previous)).map(|d| (d.total, d.from)),
        Some((Some(9), Some(DepthSource::Previous)))
    );
    assert_eq!(Curve::default().depth(None), None);
}

//! Tests of the lane decisions and caps.

use super::*;

fn counts(distinct: u64, queue: u64) -> Counts {
    Counts {
        depth: 1,
        generated: distinct * 3,
        distinct,
        queue,
    }
}

fn watch(expected: Option<Expected>) -> Watch {
    Watch::new(Settings::default(), expected, None, 0, Vec::new())
}

/// Feed one progress line a minute, `queue` at each minute given by `queue(min)`.
fn feed(w: &mut Watch, until_min: u64, queue: impl Fn(u64) -> u64) {
    for min in 1..=until_min {
        w.progress(min * 60, counts(1_000_000, queue(min)));
    }
}

#[test]
fn a_short_job_becomes_slow_at_15_minutes_then_is_probed() {
    let mut w = watch(Some(Expected::Short));
    assert_eq!(w.tick(899), Act::Run);
    assert_eq!(w.lane, Current::Short);
    assert_eq!(w.tick(900), Act::Run);
    assert_eq!(w.lane, Current::Slow(900));
    feed(&mut w, 30, |_| 200_000);
    assert_eq!(w.tick(1799), Act::Run);
    assert_eq!(w.tick(1800), Act::Run);
    assert_eq!(w.lane, Current::Probed);
    let tos: Vec<(Lane, u64, bool)> = w
        .decisions
        .iter()
        .map(|d| (d.to, d.after_secs, d.probe.is_some()))
        .collect();
    assert_eq!(tos, [(Lane::Slow, 900, false), (Lane::Slow, 1800, true)]);
    assert_eq!(w.tick(7199), Act::Run);
    assert_eq!(w.tick(7200), Act::Timeout);
}

#[test]
fn a_big_job_becomes_max_in_place_under_max_s_cap() {
    let mut w = watch(None);
    feed(&mut w, 15, |_| 500_000);
    assert_eq!(w.tick(900), Act::Run);
    assert_eq!(w.lane, Current::Max);
    let probe = Probe {
        distinct: 1_000_000,
        queue: 500_000,
        queue_before: 500_000,
    };
    assert_eq!(
        w.decisions,
        [LaneDecision {
            to: Lane::Max,
            after_secs: 900,
            probe: Some(probe)
        }]
    );
    // It keeps going under max's cap, not the slow lane's.
    assert_eq!(w.tick(7200), Act::Run);
    assert_eq!(w.tick(12 * 3600), Act::Timeout);
}

#[test]
fn a_growing_queue_is_big_too() {
    let mut w = watch(None);
    feed(&mut w, 15, |min| min * 25_000);
    assert_eq!(w.tick(900), Act::Run);
    assert_eq!(w.lane, Current::Max);
}

#[test]
fn a_probe_without_both_samples_waits_for_them() {
    // A run resumed past its probe time, its curve empty until the engine prints.
    let mut w = Watch::new(Settings::default(), None, None, 600, Vec::new());
    w.progress(280, counts(1_000_000, 900_000));
    assert_eq!(w.tick(300), Act::Run);
    assert_eq!((w.lane, w.decisions.len()), (Current::Slow(0), 0));
    w.progress(600, counts(1_000_000, 900_000));
    assert_eq!(w.tick(600), Act::Run);
    assert_eq!((w.lane, w.decisions.len()), (Current::Max, 1));
}

#[test]
fn a_timeout_counts_the_run_s_vm_time_and_replaces_lane_caps() {
    let mut w = Watch::new(
        Settings::default(),
        Some(Expected::Max),
        Some(300),
        200,
        Vec::new(),
    );
    assert_eq!(w.tick(99), Act::Run);
    assert_eq!(w.tick(100), Act::Timeout);
}

#[test]
fn a_resumed_attempt_counts_the_run_s_time_against_its_cap() {
    let mut w = Watch::new(
        Settings::default(),
        Some(Expected::Max),
        None,
        11 * 3600,
        Vec::new(),
    );
    assert_eq!(w.left(60), 3600 - 60);
    assert_eq!(w.tick(3599), Act::Run);
    assert_eq!(w.tick(3600), Act::Timeout);
}

#[test]
fn a_later_attempt_resumes_its_run_s_lane() {
    let d = |to, after_secs, probed: bool| LaneDecision {
        to,
        after_secs,
        probe: probed.then_some(Probe {
            distinct: 1,
            queue: 1,
            queue_before: 1,
        }),
    };
    let lane =
        |expected, decisions| Watch::new(Settings::default(), expected, None, 0, decisions).lane;
    assert_eq!(lane(None, vec![d(Lane::Max, 900, true)]), Current::Max);
    assert_eq!(lane(None, vec![d(Lane::Slow, 900, true)]), Current::Probed);
    assert_eq!(
        lane(Some(Expected::Short), vec![d(Lane::Slow, 900, false)]),
        Current::Slow(900)
    );
    assert_eq!(lane(Some(Expected::Max), Vec::new()), Current::Max);
    assert_eq!(lane(Some(Expected::Long), Vec::new()), Current::Slow(0));
}

#[test]
fn a_max_run_s_probe_is_shown_once_and_never_acted_on() {
    let mut w = watch(Some(Expected::Max));
    feed(&mut w, 20, |m| m * 1000);
    assert_eq!(w.tick(14 * 60), Act::Run);
    assert_eq!(w.decisions, []);
    // At 15 minutes: the numbers, recorded; the run goes on in its slot.
    assert_eq!(w.tick(15 * 60), Act::Run);
    let shown = w.decisions.clone();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].to, Lane::Max);
    assert_eq!(
        shown[0].probe,
        Some(Probe {
            distinct: 1_000_000,
            queue: 15_000,
            queue_before: 10_000,
        })
    );
    assert_eq!(w.tick(16 * 60), Act::Run);
    assert_eq!(w.decisions, shown);
    // Before any progress line, there is nothing to show yet.
    let mut early = watch(Some(Expected::Max));
    assert_eq!(early.tick(15 * 60), Act::Run);
    assert_eq!(early.decisions, []);
}

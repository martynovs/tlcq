//! The estimators replayed on a real project's full runs (`tests/fixtures/eta`, one
//! curve per run, named `<model directory>-<configuration>`), as the study did.

use super::*;

/// Every fixture run: its model directory, its name, its curve.
fn runs() -> Vec<(String, String, Curve)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eta");
    let mut out: Vec<(String, String, Curve)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|path| path.extension().is_some_and(|x| x == "tsv"))
        .map(|path| {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let model = name.split('-').next().unwrap().to_owned();
            let curve = Curve::parse(&std::fs::read_to_string(&path).unwrap());
            (model, name, curve)
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// The curve as it stood `now` seconds in: the lines printed so far.
fn cut(c: &Curve, now: f64) -> Curve {
    let points: Vec<_> = c.points.iter().copied().filter(|p| p.secs <= now).collect();
    Curve {
        left: c.left.iter().copied().take(points.len()).collect(),
        points,
        temporal: c.temporal,
        check_secs: c.check_secs.filter(|s| *s <= now),
        complete: false,
        last: None,
        end_secs: None,
    }
}

/// The population of every run but `model`'s.
fn population(all: &[(String, String, Curve)], model: &str) -> Population {
    Population::new(all.iter().filter(|r| r.0 != model).map(|r| &r.2))
}

/// The error of an estimate at `now`, as a share of the true end.
fn error(eta: Option<Eta>, now: f64, end: f64) -> Option<f64> {
    eta.map(|e| (e.end_secs.max(now) - end) / end)
}

/// The main set: full runs with 12 progress lines or more, past 15 minutes.
fn main_set(all: &[(String, String, Curve)]) -> Vec<&(String, String, Curve)> {
    all.iter()
        .filter(|r| r.2.points.len() >= 12 && r.2.end_secs.unwrap() > 900.0)
        .collect()
}

#[test]
fn a_first_run_s_estimate_at_15_minutes_is_as_close_as_the_study_found() {
    let all = runs();
    let mut errors = Vec::new();
    for (model, name, c) in main_set(&all) {
        let end = c.end_secs.unwrap();
        let eta = estimate(&cut(c, 900.0), None, &population(&all, model));
        let e = error(eta, 900.0, end).unwrap_or_else(|| panic!("{name}: no estimate"));
        assert_eq!(eta.unwrap().source, Source::FirstRun);
        errors.push(e.abs());
    }
    let within_2x = errors.iter().filter(|e| **e < 1.0).count();
    let med = median(errors.clone());
    // The study: a median error of 9 % of the total, 98 % of estimates within 2×.
    assert!(med < 0.12, "median {med}");
    assert!(
        within_2x * 100 >= errors.len() * 90,
        "{within_2x} of {}",
        errors.len()
    );
}

#[test]
fn a_rerun_of_the_same_state_space_is_estimated_within_a_few_percent() {
    let all = runs();
    let mut errors = Vec::new();
    for (model, name, c) in main_set(&all) {
        let same = all
            .iter()
            .filter(|p| &p.1 != name && p.2.last == c.last)
            .map(|p| &p.2);
        for p in same {
            let end = c.end_secs.unwrap();
            let previous = Previous {
                curve: p,
                by_name: false,
            };
            let eta = estimate(&cut(c, 900.0), Some(previous), &population(&all, model));
            assert_eq!(eta.unwrap().source, Source::Previous);
            errors.push(error(eta, 900.0, end).unwrap().abs());
        }
    }
    assert!(errors.len() >= 6, "{} pairs", errors.len());
    // The study: a median error of 2 % on real reruns at 15 minutes.
    let med = median(errors);
    assert!(med < 0.05, "median {med}");
}

#[test]
fn an_edited_model_averages_the_previous_run_and_the_first_run_estimate() {
    let all = runs();
    let (model, _, c) = main_set(&all)[0];
    let (_, _, p) = all.iter().find(|r| &r.0 == model && r.2 != *c).unwrap();
    let pop = population(&all, model);
    let now = cut(c, 900.0);
    let previous = |by_name| Previous { curve: p, by_name };
    let h = estimate(&now, Some(previous(false)), &pop).unwrap();
    let f = estimate(&now, None, &pop).unwrap();
    let edited = estimate(&now, Some(previous(true)), &pop).unwrap();
    assert_eq!(edited.source, Source::Edited);
    assert!((edited.end_secs - f64::midpoint(h.end_secs, f.end_secs)).abs() < 1e-6);
}

#[test]
fn no_estimate_comes_too_early() {
    let all = runs();
    let (model, _, c) = main_set(&all)[0];
    let pop = population(&all, model);
    assert_eq!(estimate(&cut(c, 240.0), None, &pop), None);
    assert_eq!(
        estimate(&Curve::default(), None, &Population::default()),
        None
    );
}

/// The study's own code (`tmp/eta/xcheck.py` over `est.py`) gave these ends
/// at 15 minutes and at 90 % of each run (inside the final temporal check,
/// for a run that has one): the port gives the same, to rounding.
#[test]
fn the_port_gives_the_study_s_numbers() {
    let all = runs();
    let curve = |name: &str| &all.iter().find(|r| r.1 == name).unwrap().2;
    let model = |name: &str| name.split('-').next().unwrap().to_owned();
    let study = include_str!("../tests/fixtures/eta-study.txt");
    for line in study.lines() {
        let words: Vec<&str> = line.split(' ').collect();
        let (eta, want) = match words[..] {
            ["first", now, name, want] => {
                let pop = population(&all, &model(name));
                let now = now.parse().unwrap();
                (estimate(&cut(curve(name), now), None, &pop), want)
            }
            ["hist", now, name, prev, want] => {
                let pop = population(&all, &model(name));
                let previous = Previous {
                    curve: curve(prev),
                    by_name: false,
                };
                let now = now.parse().unwrap();
                (estimate(&cut(curve(name), now), Some(previous), &pop), want)
            }
            _ => panic!("{line}"),
        };
        let want: f64 = want.parse().unwrap();
        let got = eta.unwrap().end_secs;
        assert!((got - want).abs() < 1e-6 * want, "{line}: got {got}");
    }
}

/// A synthetic curve: one line a minute from minute 1, with the counts `f`
/// gives for each minute.
fn synthetic(lines: u32, f: impl Fn(f64) -> (f64, f64, f64)) -> Curve {
    let points = (1..=lines)
        .map(|m| {
            let secs = f64::from(m) * 60.0;
            let (generated, distinct, queue) = f(secs);
            crate::curve::Point {
                secs,
                depth: u64::from(m),
                generated,
                distinct,
                queue,
            }
        })
        .collect();
    Curve {
        points,
        ..Curve::default()
    }
}

#[test]
fn a_curve_that_says_nothing_of_its_end_gives_no_estimate() {
    // Generated states that never grow: no rate to turn work into time.
    let stuck = Run::new(&synthetic(8, |_| (100.0, 50.0, 10.0)));
    assert_eq!((n4(&stuck), end_at(&stuck, 1e6)), (None, None));
    assert_eq!(h3(&stuck, &stuck), None);
    let one = Run::new(&synthetic(1, |_| (100.0, 50.0, 10.0)));
    assert_eq!(h3(&one, &one), None);
    // A line at the very start: no time to take a rate over.
    let mut at_start = synthetic(1, |_| (1.0, 1.0, 1.0));
    at_start.points[0].secs = 0.0;
    let at_start = Run::new(&at_start);
    assert!(at_start.rate(&at_start.g).abs() < f64::EPSILON);
    // A queue growing faster than the distinct states: Q/N rises, N4 has no end.
    let widening = synthetic(8, |s| (s * 100.0, s * 10.0, s * s));
    let o = Run::new(&widening);
    assert_eq!(n4(&o), None);
    assert_eq!(log_mean(&o, None, Some(5.0)), Some(5.0));
    let shaped = estimate(
        &widening,
        None,
        &Population::new(runs().iter().map(|r| &r.2)),
    );
    assert_eq!(shaped.map(|e| e.source), Some(Source::FirstRun));
}

#[test]
fn the_final_check_s_tail_comes_from_the_previous_run_or_the_population_share() {
    let all = runs();
    let (model, _, c) = main_set(&all)[0];
    let pop = population(&all, model);
    let mut checking = cut(c, 900.0);
    checking.check_secs = Some(900.0);
    // The previous run had no final check: the population's share of one.
    let previous = Previous {
        curve: c,
        by_name: false,
    };
    let eta = estimate(&checking, Some(previous), &pop).unwrap();
    assert!((eta.end_secs - 900.0 * (1.0 + pop.tail_ratio)).abs() < 1e-6);
    // A temporal configuration whose previous run had no check: H4 adds the
    // population's share.
    let mut temporal = cut(c, 900.0);
    temporal.temporal = true;
    let o = Run::new(&temporal);
    let p = Run::new(c);
    let plain = h4(&Run::new(&cut(c, 900.0)), &p, 0.5).unwrap();
    assert!((h4(&o, &p, 0.5).unwrap() - plain * 1.5).abs() < 1e-6);
}

//! Tests of the ETA's basis: the previous run and the population picked from
//! the history, and the ETA given to running lines.

use super::*;
use crate::eta::Source;
use crate::history::Record;
use crate::proto::{AttemptCurve, VmJob};

/// A fixture curve by name (`tests/fixtures/eta`).
fn fixture(name: &str) -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eta");
    std::fs::read_to_string(dir.join(format!("{name}.tsv"))).unwrap()
}

/// The fixture curve `name` as it stood at 15 minutes.
fn at_15_minutes(name: &str) -> Curve {
    let mut c = Curve::parse(&fixture(name));
    c.points.retain(|p| p.secs <= 900.0);
    Curve {
        complete: false,
        last: None,
        end_secs: None,
        check_secs: None,
        ..c
    }
}

fn keys(spec: &str, cfg_hash: &str, cfg_name: &str) -> Keys {
    Keys {
        engine: crate::engine::Engine::Tlc,
        spec_hash: format!("{spec}-{cfg_hash}"),
        cfg_hash: cfg_hash.to_owned(),
        options_hash: "o".to_owned(),
        spec: spec.to_owned(),
        cfg_name: cfg_name.to_owned(),
    }
}

/// A history record of run `end` of `id` under `keys`, its curve the fixture
/// `curve` kept as the laptop would.
fn ended(store: &Store, id: &str, keys: Keys, curve: &str) {
    std::fs::write(store.curve_path(id, "v1", 1), fixture(curve)).unwrap();
    let record = Record {
        id: id.to_owned(),
        end: "v1".to_owned(),
        names: vec![keys],
        expected: None,
        lower_bound: false,
        duration_secs: 1,
        outcome: "ok".to_owned(),
        place: "vm".to_owned(),
        curves: store.curves_of(id, "v1").unwrap(),
        lanes: Vec::new(),
    };
    History::append(&store.history(), &record).unwrap();
}

/// A store whose history holds a previous run of `NC`/`a` (the same state
/// space as the running job) and runs of other specs.
fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    ended(
        &store,
        "p",
        keys("NC", "a", "NC.cfg"),
        "november-NC_06_live_max",
    );
    for (n, other) in ["alpha-AA_03_max", "bravo-BA", "echo-EA_01"]
        .iter()
        .enumerate()
    {
        ended(&store, &format!("o{n}"), keys("Other", "x", "X.cfg"), other);
    }
    (dir, store)
}

#[test]
fn the_previous_run_is_found_by_configuration_then_by_name() {
    let (_dir, store) = store();
    let history = History::load(&store.history()).unwrap();
    let now = at_15_minutes("november-NC_05_live");
    let same = eta(&store, &history, &[keys("NC", "a", "NC.cfg")], &now)
        .unwrap()
        .unwrap();
    assert_eq!(same.source, Source::Previous);
    // The true end is 979 s: the previous run of the same space says so closely.
    assert!((same.end_secs - 979.0).abs() < 0.05 * 979.0, "{same:?}");
    let edited = eta(&store, &history, &[keys("NC", "b", "NC.cfg")], &now).unwrap();
    assert_eq!(edited.unwrap().source, Source::Edited);
    let first = eta(&store, &history, &[keys("NC", "b", "Other.cfg")], &now).unwrap();
    assert_eq!(first.unwrap().source, Source::FirstRun);
}

#[test]
fn a_tlx_job_s_estimate_never_uses_tlc_s_runs() {
    let (_dir, store) = store();
    let history = History::load(&store.history()).unwrap();
    let now = at_15_minutes("november-NC_05_live");
    let tlx = Keys {
        engine: crate::engine::Engine::Tlx,
        ..keys("NC", "a", "NC.cfg")
    };
    // The same configuration ran on TLC only: neither the previous run nor
    // the population is TLC's, so there is no basis yet.
    let got = eta(&store, &history, &[tlx], &now).unwrap();
    assert!(got.is_none_or(|e| e.source == Source::FirstRun), "{got:?}");
}

#[test]
fn a_previous_run_not_explored_to_the_end_is_passed_over() {
    let (_dir, store) = store();
    let cut = fixture("november-NC_06_live_max").replace("# complete\n", "");
    std::fs::write(
        store.curve_file(&store.curves_of("p", "v1").unwrap()[0]),
        cut,
    )
    .unwrap();
    let history = History::load(&store.history()).unwrap();
    let now = at_15_minutes("november-NC_05_live");
    let got = eta(&store, &history, &[keys("NC", "a", "NC.cfg")], &now).unwrap();
    assert_eq!(got.unwrap().source, Source::FirstRun);
}

#[test]
fn running_lines_get_an_eta_and_the_rest_none() {
    let (_dir, store) = store();
    let text = at_15_minutes("november-NC_05_live").to_jsonl();
    let job = |state| VmJob {
        state,
        run: Some("v1".to_owned()),
        entries: Vec::new(),
        place: None,
        elapsed_secs: None,
        counters: None,
        lanes: Vec::new(),
        flags: crate::proto::Flags::default(),
        curve: Some(AttemptCurve {
            attempt: 1,
            text: text.clone(),
        }),
        position: None,
        left_secs: None,
        cancelled: None,
        started_at: None,
        expected: None,
        waits_for: None,
        held: None,
    };
    std::fs::create_dir_all(store.run_dir("l", "l1")).unwrap();
    std::fs::write(store.run_dir("l", "l1").join(crate::curve::FILE), &text).unwrap();
    let running = crate::laptop_runs::Progress {
        phase: Phase::Running,
        ..Default::default()
    };
    let line = |id: &str, run: &str, view| Line {
        key: None,
        id: id.to_owned(),
        run: Some(run.to_owned()),
        view,
        eta: None,
        engine: None,
    };
    let mut lines = [
        line("v", "v1", View::Vm(job(VmState::Running))),
        line("q", "v1", View::Vm(job(VmState::Queued))),
        line("l", "l1", View::Laptop(running)),
        line(
            "w",
            "l1",
            View::Laptop(crate::laptop_runs::Progress::default()),
        ),
    ];
    let mut r = JobRecord::new("v", crate::store::State::Vm);
    r.info = Some(crate::proto::JobInfo {
        names: vec![keys("NC", "a", "NC.cfg")],
        ..crate::runner::state::tests::info()
    });
    let records = [("v".to_owned(), r)].into();
    with_etas(&store, &records, &mut lines).unwrap();
    let etas: Vec<Option<Source>> = lines.iter().map(|l| l.eta.map(|e| e.source)).collect();
    assert_eq!(
        etas,
        [Some(Source::Previous), None, Some(Source::FirstRun), None]
    );
    assert!(lines[0].eta.unwrap().remaining_secs < 200);
}

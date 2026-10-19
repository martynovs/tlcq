//! Tests of the runner's page: what it shows, and its cancel and reorder.

use std::fmt::Write as _;

use super::*;
use crate::commit::CommitRoute;
use crate::runner::state::tests::run;
use crate::runner::verbs::tests::{job, runner, wire, Fake};

/// A request.
fn request(method: &str, path: &str, body: &str) -> Request {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    Request {
        method: method.to_owned(),
        path: path.to_owned(),
        query: query
            .split('&')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect(),
        body: body.as_bytes().to_vec(),
        content_type: (method == "POST").then(|| "application/json; charset=utf-8".to_owned()),
    }
}

fn get(r: &mut Runner<Fake>, path: &str) -> Response {
    handle(r, &mut Page::default(), &request("GET", path, ""))
}

fn post(r: &mut Runner<Fake>, path: &str, body: &serde_json::Value) -> Response {
    handle(
        r,
        &mut Page::default(),
        &request("POST", path, &body.to_string()),
    )
}

fn json(res: &Response) -> serde_json::Value {
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    serde_json::from_slice(&res.body).unwrap()
}

fn state(r: &mut Runner<Fake>) -> serde_json::Value {
    let res = get(r, "/api/state");
    assert_eq!(res.status, 200);
    serde_json::from_slice(&res.body).unwrap()
}

/// A fixture curve (`tests/fixtures/eta`).
fn fixture(name: &str) -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eta");
    std::fs::read_to_string(dir.join(format!("{name}.tsv"))).unwrap()
}

/// A runner with the P slot closed, job `A` running in E and `B`, `C` queued behind it.
fn three() -> (tempfile::TempDir, Runner<Fake>, [String; 3]) {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _fake) = runner(dir.path());
    r.set_p_open(false).unwrap();
    let jobs = ["A", "B", "C"].map(|s| job(s, CommitRoute::Queue));
    let ids = jobs.clone().map(|(j, _)| j.id);
    r.commit(wire("t1", jobs.to_vec(), Vec::new()).as_slice())
        .unwrap();
    (dir, r, ids)
}

/// Change the runner's state.
fn edit(r: &Runner<Fake>, f: impl FnOnce(&mut State)) {
    let lock = r.store.lock().unwrap();
    let mut state = r.store.load(&lock).unwrap();
    f(&mut state);
    r.store.save(&lock, &state).unwrap();
}

#[test]
fn the_page_shows_the_slots_the_queue_in_order_and_a_run_s_progress() {
    let (_dir, mut r, [first, second, third]) = three();
    let attempt = r.store.attempt_dir(&first, "v1", 1);
    let mut curve = Curve::parse(&fixture("alpha-AC_01"));
    curve.points.retain(|p| p.secs <= 900.0);
    std::fs::write(attempt.join(crate::curve::FILE), curve.to_jsonl()).unwrap();
    let so_far = AttemptResult {
        duration_secs: 900,
        started_at: Some(1_700_000_000),
        left_secs: Some(6300),
        ..AttemptResult::default()
    };
    crate::files::write_json(&attempt.join("result.json"), &so_far).unwrap();
    let v = state(&mut r);
    assert_eq!(v["running"][0]["started_at"], 1_700_000_000);
    assert_eq!(v["free_memory_mb"], 8192);
    assert_eq!(
        (&v["room_mb"], &v["min_mb"], &v["shares"]["whole_mb"]),
        (&6144.into(), &4096.into(), &16_384.into())
    );
    // A holds E; P is closed and free.
    assert_eq!(v["slots"].as_array().unwrap().len(), 2);
    assert_eq!(
        (
            v["p_open"].as_bool(),
            v["slots"][1]["open"].as_bool(),
            v["slots"][1]["holder"].as_str()
        ),
        (Some(false), Some(false), Some("free"))
    );
    assert_eq!(
        (
            v["slots"][0]["name"].as_str(),
            v["slots"][0]["holder"].as_str()
        ),
        (Some("E"), Some("job"))
    );
    assert_eq!(v["slots"][0]["run"], format!("{first} v1"));
    let queued: Vec<(&str, u64, bool)> = v["queue"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| {
            let id = q["id"].as_str().unwrap();
            (
                id,
                q["position"].as_u64().unwrap(),
                q["movable"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        queued,
        [(second.as_str(), 1, true), (third.as_str(), 2, true)]
    );
    let running = &v["running"][0];
    assert_eq!(
        (running["id"].as_str(), running["state"].as_str()),
        (Some(first.as_str()), Some("running"))
    );
    assert_eq!(running["left_secs"], 6300);
    let distinct = curve.points.last().unwrap().distinct;
    assert_eq!(running["counters"]["distinct"].as_f64(), Some(distinct));
    assert!(running["generated_per_min"].as_f64().unwrap() > 0.0);
    assert!(running["queue_share"].as_f64().unwrap() > 0.0);
    assert!(running["queue_growth"].is_number());
    assert!(running["queue_per_min"].is_number());
    // The fixture's 15 minutes give a slope at each line of the last 10.
    assert!(
        running["queue_slopes"]
            .as_array()
            .is_some_and(|s| s.len() >= 5),
        "{running}"
    );
    assert_eq!(running["configs"][0], "A.cfg");
}

#[test]
fn a_cancel_from_the_page_is_by_ui_and_a_reorder_moves_a_queued_run() {
    let (_dir, mut r, [_, b, c]) = three();
    let moved = post(
        &mut r,
        "/api/reorder",
        &serde_json::json!({"id": c, "run": "v1", "to": "head"}),
    );
    assert_eq!((moved.status, moved.body.as_slice()), (200, &b"true"[..]));
    let order: Vec<String> = state(&mut r)["queue"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| q["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(order, [c.clone(), b.clone()]);
    let cancelled = post(
        &mut r,
        "/api/cancel",
        &serde_json::json!({"id": b, "run": "v1"}),
    );
    assert_eq!(cancelled.status, 200);
    assert_eq!(state(&mut r)["ended_total"], 1);
    let page = json(&get(&mut r, "/api/ended"));
    let ended = &page["runs"][0];
    assert_eq!(
        (ended["id"].as_str(), ended["result"]["by"].as_str()),
        (Some(b.as_str()), Some("ui"))
    );
    // Ended while queued, with no attempt: its end time is the state's.
    assert!(ended["ended_at"]
        .as_u64()
        .is_some_and(|t| t > 1_700_000_000));
    // A post that is not JSON, as a cross-site form would send, does nothing.
    let mut form = request(
        "POST",
        "/api/cancel",
        &serde_json::json!({"id": c, "run": "v1"}).to_string(),
    );
    form.content_type = Some("text/plain".to_owned());
    assert_eq!(handle(&mut r, &mut Page::default(), &form).status, 415);
    form.content_type = None;
    assert_eq!(handle(&mut r, &mut Page::default(), &form).status, 415);
    assert_eq!(state(&mut r)["ended_total"], 1);
    assert_eq!(
        post(&mut r, "/api/cancel", &serde_json::json!({"id": 1})).status,
        400
    );
    assert_eq!(
        post(&mut r, "/api/reorder", &serde_json::json!({})).status,
        400
    );
}

#[test]
fn a_hold_from_the_page_holds_the_queue_and_a_release_admits_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    assert_eq!(state(&mut r)["held"].as_bool(), Some(false));
    assert_eq!(
        json(&post(&mut r, "/api/hold", &serde_json::json!({}))),
        true
    );
    r.commit(wire("t1", vec![job("A", CommitRoute::Queue)], Vec::new()).as_slice())
        .unwrap();
    let paused = state(&mut r);
    assert_eq!(
        (
            paused["held"].as_bool(),
            paused["queue"].as_array().map(Vec::len)
        ),
        (Some(true), Some(1))
    );
    assert_eq!(
        json(&post(&mut r, "/api/release", &serde_json::json!({}))),
        true
    );
    let resumed = state(&mut r);
    assert_eq!(
        (
            resumed["held"].as_bool(),
            resumed["running"].as_array().map(Vec::len)
        ),
        (Some(false), Some(1))
    );
    assert_eq!(fake.0.borrow().started.len(), 1);
}

#[test]
fn the_page_its_logs_and_unknown_paths() {
    let (_dir, mut r, [a, ..]) = three();
    std::fs::write(
        r.store.attempt_dir(&a, "v1", 1).join("tlc.log"),
        "Starting...",
    )
    .unwrap();
    let log = get(&mut r, &format!("/api/output?id={a}&run=v1&attempt=1"));
    assert_eq!(
        (log.status, log.body.as_slice()),
        (200, &b"Starting..."[..])
    );
    assert_eq!(
        get(&mut r, &format!("/api/output?id={a}&run=v1&attempt=2")).status,
        404
    );
    for bad in [
        "id=..&run=v1&attempt=1",
        "id=a&run=x1&attempt=1",
        "id=a&run=v&attempt=1",
        "id=a&run=v1",
    ] {
        assert_eq!(
            get(&mut r, &format!("/api/output?{bad}")).status,
            400,
            "{bad}"
        );
    }
    assert_eq!(get(&mut r, "/nowhere").status, 404);
    // A state that cannot be read is a 500, saying why.
    std::fs::write(r.store.attempt_dir(&a, "v1", 1).join("tlc.log"), "").unwrap();
    std::fs::remove_file(r.store.attempt_dir(&a, "v1", 1).join("tlc.log")).unwrap();
    std::fs::create_dir_all(r.store.attempt_dir(&a, "v1", 1).join("tlc.log")).unwrap();
    assert_eq!(
        get(&mut r, &format!("/api/output?id={a}&run=v1&attempt=1")).status,
        500
    );
}

#[test]
fn a_run_s_progress_curve_comes_for_its_charts_thinned_to_a_few_hundred_points() {
    let (_dir, mut r, [a, ..]) = three();
    let path = |n| format!("/api/curve?id={a}&run=v1&attempt={n}");
    assert_eq!(get(&mut r, &path(1)).status, 404);
    let text = (0..1000u32).fold(String::new(), |mut t, m| {
        writeln!(t, "{}\t1\t{}\t{}\t{}", m * 60, m * 10, m * 5, m).unwrap();
        t
    });
    std::fs::write(
        r.store.attempt_dir(&a, "v1", 1).join("curve.tsv"),
        format!("# tlcq curve\n{text}"),
    )
    .unwrap();
    let points = json(&get(&mut r, &path(1)));
    let points = points.as_array().unwrap();
    assert!(
        points.len() <= 400 && points.len() > 300,
        "{}",
        points.len()
    );
    assert_eq!(points[0], serde_json::json!([0.0, 0.0, 0.0, 0.0]));
    // The last line is always there.
    assert_eq!(
        points.last().unwrap(),
        &serde_json::json!([999.0, 9990.0, 4995.0, 999.0])
    );
    assert_eq!(get(&mut r, "/api/curve?id=..&run=v1&attempt=1").status, 400);
}

#[test]
fn a_running_run_s_eta_comes_from_a_previous_run_of_its_configuration_else_its_own_curve() {
    let (_dir, mut r, [a, ..]) = three();
    let mut now = Curve::parse(&fixture("alpha-AC_01"));
    now.points.retain(|p| p.secs <= 900.0);
    std::fs::write(
        r.store.attempt_dir(&a, "v1", 1).join(crate::curve::FILE),
        now.to_jsonl(),
    )
    .unwrap();
    // Only another spec's run ended: the first-run estimate.
    let ended = |id: &str, spec: &str, cfg: &str| {
        let mut p = run(
            "v1",
            Phase::Ended {
                result: State::outcome_only(crate::outcome::Outcome::Ok),
            },
            cfg,
        );
        p.info.spec = spec.to_owned();
        p.info.names[0].spec = spec.to_owned();
        p.info.names[0].cfg_hash = cfg.to_owned();
        p.attempts = vec![ended_attempt()];
        (id.to_owned(), p)
    };
    for (id, p, curve) in [
        (ended("o1", "Other", "x"), "bravo-BA"),
        (ended("o2", "Other", "y"), "echo-EA_01"),
        (ended("o3", "Other", "z"), "alpha-AA_03_max"),
    ]
    .map(|((id, p), c)| (id, p, c))
    {
        std::fs::create_dir_all(r.store.attempt_dir(&id, "v1", 1)).unwrap();
        std::fs::write(
            r.store.attempt_dir(&id, "v1", 1).join("curve.tsv"),
            fixture(curve),
        )
        .unwrap();
        edit(&r, |s| {
            s.runs.insert(id.clone(), vec![p]);
        });
    }
    // An ended run that never ran, and one not explored to the end, count for nothing.
    let (never, mut never_ran) = ended("n1", "Other", "w");
    never_ran.attempts.clear();
    let (cut, unexplored) = ended("n2", "Other", "v");
    std::fs::create_dir_all(r.store.attempt_dir(&cut, "v1", 1)).unwrap();
    let unfinished = fixture("bravo-BA").replace("# complete\n", "");
    std::fs::write(
        r.store.attempt_dir(&cut, "v1", 1).join("curve.tsv"),
        unfinished,
    )
    .unwrap();
    edit(&r, |s| {
        s.runs.insert(never.clone(), vec![never_ran]);
        s.runs.insert(cut.clone(), vec![unexplored]);
    });
    let v = state(&mut r);
    assert_eq!(v["running"][0]["eta_source"], "first_run");
    assert_eq!(v["running"][0]["last_secs"], serde_json::Value::Null);
    // A previous run of the same configuration: its estimate.
    let (id, mut p) = ended("p1", "M", "c");
    p.info.names[0].spec_hash = "s".to_owned();
    if let Phase::Ended { result } = &mut p.phase {
        result.duration_secs = 2091;
    }
    std::fs::create_dir_all(r.store.attempt_dir(&id, "v1", 1)).unwrap();
    std::fs::write(
        r.store.attempt_dir(&id, "v1", 1).join("curve.tsv"),
        fixture("alpha-AC_02_max"),
    )
    .unwrap();
    edit(&r, |s| {
        s.runs.insert(id.clone(), vec![p]);
    });
    let v = state(&mut r);
    assert_eq!(v["running"][0]["eta_source"], "previous");
    // Its whole duration is the last known time.
    assert_eq!(v["running"][0]["last_secs"], 2091);
    // Each run names its engine (the fixture's jobs are TLC's).
    assert_eq!(v["running"][0]["engine"], "tlc");
    // The run ends 2091 s in; the estimate, 15 minutes in, is near it.
    let eta = v["running"][0]["eta_secs"].as_u64().unwrap();
    assert!((900..1500).contains(&eta), "{eta}");
    // Pausing in its slot, it still runs: the same estimate.
    pausing(&r, true);
    let v = state(&mut r);
    let row = &v["running"][0];
    assert_eq!(
        (row["state"].as_str(), row["eta_secs"].as_u64()),
        (Some("pausing"), Some(eta))
    );
    pausing(&r, false);
    // The page reads an ended attempt's curve once: gone from disk, it still
    // counts for the page that read it, not for a fresh one.
    let source = |page: &mut Page| view(&r, page).unwrap().running[0].eta_source;
    let mut page = Page::default();
    assert_eq!(source(&mut page), Some(Source::Previous));
    std::fs::remove_file(r.store.attempt_dir(&id, "v1", 1).join("curve.tsv")).unwrap();
    assert_eq!(source(&mut page), Some(Source::Previous));
    assert_eq!(source(&mut Page::default()), Some(Source::FirstRun));
}

#[test]
fn the_built_app_is_served_from_its_directory_its_routes_by_its_index() {
    let (_dir, mut r, _) = three();
    let site = tempfile::tempdir().unwrap();
    std::fs::create_dir(site.path().join("assets")).unwrap();
    std::fs::write(site.path().join("index.html"), "<!doctype html>app").unwrap();
    std::fs::write(site.path().join("assets/app.js"), "go()").unwrap();
    let mut page = Page::new(Some(site.path().to_path_buf()));
    let mut at = |path: &str| handle(&mut r, &mut page, &request("GET", path, ""));
    for (path, kind, body) in [
        ("/", "text/html", "<!doctype html>app"),
        ("/ended", "text/html", "<!doctype html>app"),
        ("/assets/app.js", "text/javascript", "go()"),
    ] {
        let res = at(path);
        assert_eq!(res.status, 200, "{path}");
        assert!(res.content_type.starts_with(kind), "{path}");
        assert_eq!(res.body, body.as_bytes(), "{path}");
    }
    for missing in [
        "/assets/none.css",
        "/../secret.txt",
        "/.git/config",
        "/api/none",
    ] {
        assert_eq!(at(missing).status, 404, "{missing}");
    }
    std::fs::create_dir(site.path().join("dir.js")).unwrap();
    assert_eq!(at("/dir.js").status, 500);
    // No built app: every page is a 404 saying how to install it.
    let none = get(&mut r, "/");
    assert_eq!(none.status, 404);
    assert!(String::from_utf8_lossy(&none.body).contains("TLCQ_UI_DIR"));
    let types: Vec<&str> = [
        "x.css", "x.svg", "x.json", "x.png", "x.ico", "x.woff2", "x.bin",
    ]
    .map(|f| content_type(std::path::Path::new(f)))
    .to_vec();
    assert_eq!(
        types,
        [
            "text/css; charset=utf-8",
            "image/svg+xml",
            "application/json",
            "image/png",
            "image/x-icon",
            "font/woff2",
            "application/octet-stream"
        ]
    );
}

#[test]
fn ended_runs_come_a_page_at_a_time_the_latest_end_first() {
    let (_dir, mut r, ids) = three();
    // Each has ended; the middle one's end time is unknown.
    edit(&r, |s| {
        for runs in s.runs.values_mut() {
            runs[0].phase = Phase::Ended {
                result: State::outcome_only(crate::outcome::Outcome::Ok),
            };
            runs[0].attempts = vec![ended_attempt()];
        }
    });
    for (i, id) in ids.iter().enumerate() {
        if i != 1 {
            let path = r.store.attempt_dir(id, "v1", 1).join("result.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let end = AttemptResult {
                ended_at: Some(1000 + i as u64),
                ..AttemptResult::default()
            };
            crate::files::write_json(&path, &end).unwrap();
        }
    }
    let order = |page: &serde_json::Value| -> Vec<String> {
        page["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let first = json(&get(&mut r, "/api/ended?page=1&per=2"));
    assert_eq!(
        (first["total"].as_u64(), first["per"].as_u64()),
        (Some(3), Some(2))
    );
    assert_eq!(order(&first), [ids[2].clone(), ids[0].clone()]);
    assert_eq!(first["runs"][0]["ended_at"], 1002);
    let second = json(&get(&mut r, "/api/ended?page=2&per=2"));
    assert_eq!(order(&second), [ids[1].clone()]);
    // Out of range or garbled: clamped, or the defaults.
    assert_eq!(
        order(&json(&get(&mut r, "/api/ended?page=9"))),
        Vec::<String>::new()
    );
    let clamped = json(&get(&mut r, "/api/ended?page=0&per=999"));
    assert_eq!(
        (clamped["page"].as_u64(), clamped["per"].as_u64()),
        (Some(1), Some(100))
    );
    assert_eq!(json(&get(&mut r, "/api/ended?per=x"))["per"], 20);
}

/// An attempt 1 in the E slot, ended.
fn ended_attempt() -> crate::runner::state::Attempt {
    crate::runner::state::Attempt {
        ended: true,
        ..crate::runner::state::Attempt::new(1, Slot::E, false)
    }
}

#[test]
#[expect(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    reason = "one walk through a run's states on the page, with the jobs a, b and c as in the other tests"
)]
fn frozen_and_paused_runs_show_their_state_slot_and_actions_and_a_stopping_run_its_canceller() {
    let (_dir, mut r, [a, b, c]) = three();
    // Not started yet: nothing to do but cancel.
    let v = state(&mut r);
    assert_eq!(v["running"][0]["actions"], serde_json::json!([]));
    std::fs::write(r.store.attempt_dir(&a, "v1", 1).join("started"), "").unwrap();
    let v = state(&mut r);
    assert_eq!(
        v["running"][0]["actions"],
        serde_json::json!(["freeze", "pause"])
    );
    // Frozen, A frees E: B takes it.
    let named = serde_json::json!({ "id": a, "run": "v1" });
    assert_eq!(json(&post(&mut r, "/api/freeze", &named)), true);
    let v = state(&mut r);
    let slots: Vec<(&str, &str, &str)> = v["slots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap(),
                s["holder"].as_str().unwrap(),
                s["run"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        slots,
        [("E", "job", format!("{b} v1").as_str()), ("P", "free", "")]
    );
    let frozen = |v: &serde_json::Value| {
        v["running"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == a.as_str())
            .unwrap()
            .clone()
    };
    let f = frozen(&v);
    assert_eq!(
        (
            f["state"].as_str(),
            f["actions"].clone(),
            f["since"].is_u64()
        ),
        (
            Some("frozen"),
            serde_json::json!(["unfreeze", "pause"]),
            true
        )
    );
    // C cancelled, then P opened: spare capacity, A (not held) thaws there.
    crate::runner::verbs::tests::unhold(&r, &a);
    json(&post(
        &mut r,
        "/api/cancel",
        &serde_json::json!({ "id": c, "run": "v1" }),
    ));
    assert_eq!(
        json(&post(
            &mut r,
            "/api/p-slot",
            &serde_json::json!({ "open": true })
        )),
        true
    );
    let v = state(&mut r);
    assert_eq!(
        (v["slots"][1]["holder"].as_str(), v["p_open"].as_bool()),
        (Some("thawed"), Some(true))
    );
    assert_eq!(frozen(&v)["state"], "thawed");
    assert_eq!(frozen(&v)["place"], "vm P");
    assert_eq!(json(&post(&mut r, "/api/unfreeze", &named)), true);
    assert_eq!(frozen(&state(&mut r))["state"], "running");
    // Pausing, kept running, pausing again, then paused at a checkpoint:
    // resume is all it offers.
    assert_eq!(json(&post(&mut r, "/api/pause", &named)), true);
    assert_eq!(json(&post(&mut r, "/api/pause", &named)), false);
    let f = frozen(&state(&mut r));
    assert_eq!(
        (f["state"].as_str(), f["actions"].clone()),
        (Some("pausing"), serde_json::json!(["keep"]))
    );
    assert_eq!(json(&post(&mut r, "/api/keep", &named)), true);
    assert_eq!(frozen(&state(&mut r))["state"], "running");
    assert_eq!(json(&post(&mut r, "/api/pause", &named)), true);
    edit(&r, |s| {
        let run = &mut s.runs.get_mut(&a).unwrap()[0];
        run.attempts[0].ended = true;
        run.attempts[0].paused = true;
    });
    let dir = r.store.attempt_dir(&a, "v1", 1);
    let at = AttemptResult {
        ended_at: Some(1_700_000_500),
        ..AttemptResult::default()
    };
    crate::files::write_json(&dir.join("result.json"), &at).unwrap();
    // Its disk is its kept checkpoint's.
    let kept = r.store.checkpoint_dir(&a, "v1").join("tlc");
    std::fs::create_dir_all(&kept).unwrap();
    std::fs::write(kept.join("fpset-0"), vec![1; 3 << 20]).unwrap();
    let f = frozen(&state(&mut r));
    assert_eq!(
        (
            f["state"].as_str(),
            f["actions"].clone(),
            f["since"].as_u64(),
            f["scratch_mb"].as_u64()
        ),
        (
            Some("paused"),
            serde_json::json!(["resume"]),
            Some(1_700_000_500),
            Some(3)
        )
    );
    // Resumed with P free: its next attempt starts at once.
    assert_eq!(json(&post(&mut r, "/api/resume", &named)), true);
    let f = frozen(&state(&mut r));
    assert_eq!(
        (f["state"].as_str(), f["attempt"].as_u64()),
        (Some("running"), Some(2))
    );
    // A garbled request, and a stopping run's canceller.
    assert_eq!(
        post(&mut r, "/api/freeze", &serde_json::json!({})).status,
        400
    );
    assert_eq!(
        post(&mut r, "/api/p-slot", &serde_json::json!({})).status,
        400
    );
    edit(&r, |s| {
        s.runs.get_mut(&a).unwrap()[0].phase = Phase::Stopping {
            by: "w1".to_owned(),
            replaced_by: None,
        };
    });
    let f = frozen(&state(&mut r));
    assert_eq!(
        (f["state"].as_str(), f["cancelled_by"].as_str()),
        (Some("stopping"), Some("w1"))
    );
}

#[test]
fn a_live_run_s_state_and_actions_follow_its_attempt() {
    use crate::runner::state::{Attempt, Freeze};
    let with = |attempt: Option<Attempt>, resume: bool, started: bool| {
        let mut r = run("v1", crate::runner::state::tests::running(), "A.cfg");
        r.resume = resume;
        r.attempts.extend(attempt);
        run_state(&r, started, false)
    };
    let live = |freeze, pausing| {
        Some(Attempt {
            freeze,
            pausing,
            ..Attempt::new(1, Slot::E, false)
        })
    };
    assert_eq!(with(None, false, false), ("running", vec![]));
    assert_eq!(with(None, true, false), ("resuming", vec![]));
    assert_eq!(
        with(live(Freeze::Running, false), false, false),
        ("running", vec![])
    );
    assert_eq!(
        with(live(Freeze::Running, false), false, true),
        ("running", vec!["freeze", "pause"])
    );
    assert_eq!(
        with(live(Freeze::Running, true), false, true),
        ("pausing", vec!["keep"])
    );
    assert_eq!(
        with(live(Freeze::Unpaused, false), false, true),
        ("pausing", vec![])
    );
    assert_eq!(
        with(live(Freeze::Thawed, false), false, true),
        ("thawed", vec!["unfreeze", "pause"])
    );
    assert_eq!(
        with(live(Freeze::Thawed, true), false, true),
        ("pausing", vec!["keep"])
    );
    assert_eq!(
        with(live(Freeze::Frozen, false), false, false),
        ("frozen", vec!["unfreeze"])
    );
    assert_eq!(
        with(live(Freeze::Unfrozen, false), false, true),
        ("unfrozen", vec!["pause"])
    );
    // Once its search has ended, no pause is offered.
    let mut r = run("v1", crate::runner::state::tests::running(), "A.cfg");
    r.attempts.extend(live(Freeze::Running, false));
    assert_eq!(run_state(&r, true, true), ("running", vec!["freeze"]));
}

#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "the jobs a, b and c, as in the other tests"
)]
fn a_freeze_holds_the_job_frozen_with_a_slot_free_and_a_pause_that_never_lands_says_so() {
    let (_dir, mut r, [a, b, c]) = three();
    for id in [&b, &c] {
        json(&post(
            &mut r,
            "/api/cancel",
            &serde_json::json!({ "id": id, "run": "v1" }),
        ));
    }
    std::fs::write(r.store.attempt_dir(&a, "v1", 1).join("started"), "").unwrap();
    // Frozen with E free and nothing waiting: it stays frozen, in no slot,
    // until unfrozen.
    let named = serde_json::json!({ "id": a, "run": "v1" });
    assert_eq!(json(&post(&mut r, "/api/freeze", &named)), true);
    let v = state(&mut r);
    let f = &v["running"][0];
    assert_eq!(
        (f["state"].as_str(), f["slot"].as_str()),
        (Some("frozen"), None)
    );
    assert_eq!(json(&post(&mut r, "/api/unfreeze", &named)), true);
    let v = state(&mut r);
    assert_eq!(v["running"][0]["slot"].as_str(), Some("E"));
    // Paused after its search ended: the pause never lands.
    assert_eq!(json(&post(&mut r, "/api/pause", &named)), true);
    std::fs::write(r.store.attempt_dir(&a, "v1", 1).join("searched"), "").unwrap();
    let v = state(&mut r);
    let f = &v["running"][0];
    assert_eq!(
        (
            f["state"].as_str(),
            f["slot"].as_str(),
            f["no_checkpoint"].as_bool()
        ),
        (Some("pausing"), Some("E"), Some(true))
    );
}

#[test]
fn a_whole_vm_run_holds_p_empty_on_the_page_and_shows_its_memory() {
    use crate::runner::state::tests::running;
    use crate::runner::state::{Attempt, State};
    let mut s = State::default();
    let mut w = run("v1", running(), "A.cfg");
    w.info.memory_mb = Some(12_000);
    w.last_attempt = 1;
    w.attempts.push(Attempt {
        memory_mb: Some(16_384),
        whole: true,
        ..Attempt::new(1, Slot::E, false)
    });
    s.runs.insert("w".to_owned(), vec![w]);
    let views = super::slots(&s, true);
    let held: Vec<(&str, &str)> = views
        .iter()
        .map(|v| (v.holder, v.run.as_deref().unwrap_or("")))
        .collect();
    assert_eq!(held, [("job", "w v1"), ("whole", "w v1")]);
    // Paused, it holds nothing.
    s.runs.get_mut("w").unwrap()[0].attempts[0].ended = true;
    let held: Vec<&str> = super::slots(&s, true).iter().map(|v| v.holder).collect();
    assert_eq!(held, ["free", "free"]);
}

/// Mark every live attempt pausing, or not.
fn pausing(r: &Runner<Fake>, on: bool) {
    edit(r, |s| {
        for run in s.runs.values_mut().flatten() {
            if let Some(a) = run.attempts.last_mut().filter(|a| !a.ended) {
                a.pausing = on;
            }
        }
    });
}

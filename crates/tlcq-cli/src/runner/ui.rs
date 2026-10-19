//! The web page on the runner (design D16, pinned-slots S6): what holds each
//! slot and whether P is open, the queue in its order, each running, frozen or
//! paused job's progress, time left and ETA, ended jobs with their outcome and
//! log, and every job's submitters; and a human's cancel (by `ui`), reorder,
//! freeze, unfreeze, pause, resume, P toggle and hold of the queue, each the
//! runner's own verb under the admission lock. The page itself is a static
//! app built from `ui/` and served from `TLCQ_UI_DIR`; it asks for
//! `/api/state` every few seconds, and for ended runs a page at a time
//! (`/api/ended`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::admit::Move;
use super::http::{Request, Response};
use super::state::{Freeze, Phase, Run, Slot, State};
use super::store::{AttemptResult, Store};
use super::verbs::{Containers, Runner};
use crate::curve::Curve;
use crate::engine::Engine;
use crate::eta::{estimate, Population, Previous, Source};
use crate::files::read_json;
use crate::history::{Expected, Keys};
use crate::outcome::{JobResult, LaneDecision};
use crate::proto::{CancelRequest, Counters};

/// Ended runs per page, unless the page asks for another number.
const PER_PAGE: usize = 20;

/// The most ended runs one page holds.
const MAX_PER_PAGE: usize = 100;

/// The window of the rates and the queue's growth, in seconds.
const WINDOW: f64 = 300.0;

/// The most points a progress chart gets.
const CURVE_POINTS: usize = 400;

/// How far back the queue's slope is shown, in seconds.
const SLOPES_SPAN: f64 = 3600.0;

/// What the page shows.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct View {
    /// What holds each slot.
    pub slots: Vec<SlotView>,
    /// The queued runs, in order.
    pub queue: Vec<RunView>,
    /// The running runs (frozen, paused, resuming and stopping ones included).
    pub running: Vec<RunView>,
    /// How many runs have ended (they come a page at a time: [`Ended`]).
    pub ended_total: usize,
    /// Whether the queue is held: the stopping mark (D10), until a release.
    pub held: bool,
    /// Whether the P slot is open.
    pub p_open: bool,
    /// What the queued runs wait for: `the queue to be released`, `disk`,
    /// `memory` (frozen jobs may hold it), `the whole VM` or `a VM slot`.
    pub waits_for: Option<&'static str>,
    /// The state volume's free space and floor.
    pub disk: Option<crate::proto::Disk>,
    /// The VM's free memory, in MB.
    pub free_memory_mb: Option<u64>,
    /// The memory a new attempt could start with now, in MB, before its
    /// slot's share caps it.
    pub room_mb: Option<u64>,
    /// The most an attempt gets, in MB: in a slot with both open, and in E
    /// alone (a whole-VM request, or the P slot closed).
    pub shares: Option<super::admit::Shares>,
    /// The least memory an attempt starts with, in MB.
    pub min_mb: u64,
}

/// A page of ended runs, the latest end first.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Ended {
    /// How many runs have ended.
    pub total: usize,
    /// The page, from 1.
    pub page: usize,
    /// Runs per page.
    pub per: usize,
    /// This page's runs.
    pub runs: Vec<RunView>,
}

/// One slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SlotView {
    /// Its name: `E` or `P`.
    pub name: &'static str,
    /// Whether it is open (E always is).
    pub open: bool,
    /// What holds it: `job`, `thawed` (a frozen job on spare capacity), `whole`
    /// (P only: kept empty for a whole-VM run's memory) or `free`.
    pub holder: &'static str,
    /// The run holding it, as `<id> <run>`.
    pub run: Option<String>,
}

/// One run.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RunView {
    /// The job.
    pub id: String,
    /// The run.
    pub run: String,
    /// The spec module.
    pub spec: String,
    /// The engine it runs on.
    pub engine: Engine,
    /// Its configuration names.
    pub configs: Vec<String>,
    /// Its submitters: each entry's label.
    pub submitters: Vec<String>,
    /// `queued`, `running`, `frozen`, `thawed` (frozen, on spare capacity),
    /// `unfrozen` (waiting first for a slot), `pausing`, `paused`, `resuming`
    /// (waiting first for a slot), `stopping` or `ended`.
    pub state: &'static str,
    /// What a human may do to it besides cancel: `freeze`, `unfreeze`,
    /// `pause`, `resume`.
    pub actions: Vec<&'static str>,
    /// Since when it is frozen, or when it stopped at its checkpoint if
    /// paused, in seconds since the epoch.
    pub since: Option<u64>,
    /// The slot it holds now (`E` or `P`): none while frozen, paused, waiting,
    /// or unpaused for its pause.
    pub slot: Option<&'static str>,
    /// Pausing after its search ended: no checkpoint comes, it ends on its own.
    pub no_checkpoint: bool,
    /// Paused on its checkpoint because it died (a long run, or a resume), and why.
    pub died: Option<String>,
    /// The memory its live attempt started with, in MB.
    pub memory_mb: Option<u64>,
    /// The disk its live attempt's scratch takes (the engine's spill and
    /// checkpoint), or a paused run's kept checkpoint, in MB.
    pub scratch_mb: Option<u64>,
    /// Its memory request, in MB (`--memory`).
    pub request_mb: Option<u64>,
    /// Its place among the queued runs, from 1.
    pub position: Option<usize>,
    /// Whether a human may move it: queued, with no recorded attempt.
    pub movable: bool,
    /// Its place: `vm` and the slot.
    pub place: Option<String>,
    /// Its last attempt.
    pub attempt: Option<u32>,
    /// How long it has run, in seconds.
    pub elapsed_secs: Option<u64>,
    /// TLC's latest counters.
    pub counters: Option<Counters>,
    /// Generated states per minute, over the last 5 minutes.
    pub generated_per_min: Option<f64>,
    /// Distinct states per minute, over the last 5 minutes.
    pub distinct_per_min: Option<f64>,
    /// The queue as a share of the distinct states.
    pub queue_share: Option<f64>,
    /// The queue's growth over the last 5 minutes, as a share of the distinct states.
    pub queue_growth: Option<f64>,
    /// The queue's slope, states per minute, fitted over the last 5 minutes.
    pub queue_per_min: Option<f64>,
    /// That slope at each progress line of the last hour, oldest first:
    /// whether it grows, holds or shrinks.
    pub queue_slopes: Vec<f64>,
    /// tlx's level being expanded: what is left of it (the queue counts the
    /// next level found so far too), and its change over the last 5 minutes.
    pub level: Option<crate::curve::Level>,
    /// Its depth now and how deep it is likely to go.
    pub depth: Option<crate::curve::Depth>,
    /// The expected duration it was placed with.
    pub expected: Option<Expected>,
    /// Where that came from.
    pub expected_source: Option<crate::history::Source>,
    /// Its lane decisions, with the probe's numbers.
    pub lanes: Vec<LaneDecision>,
    /// Seconds left before its cap.
    pub left_secs: Option<u64>,
    /// Its estimated end: the seconds left after TLC's last progress line.
    pub eta_secs: Option<u64>,
    /// Where that estimate comes from.
    pub eta_source: Option<Source>,
    /// The last known time: the whole duration of the run the estimate reads,
    /// the latest complete one of the same configuration.
    pub last_secs: Option<u64>,
    /// Who cancelled it, while it stops.
    pub cancelled_by: Option<String>,
    /// Its result, once ended.
    pub result: Option<JobResult>,
    /// When its running attempt's TLC started, in seconds since the epoch.
    pub started_at: Option<u64>,
    /// When its last attempt ended, in seconds since the epoch.
    pub ended_at: Option<u64>,
}

/// What the page's thread keeps across requests.
#[derive(Debug, Default)]
pub struct Page {
    /// The built app (`TLCQ_UI_DIR`), if any.
    pub site: Option<PathBuf>,
    /// Ended attempts' curves by attempt directory, read once: attempt numbers are never
    /// reused, so an ended attempt's curve never changes, and a poll need not
    /// read every ended run's again for the ETAs.
    curves: HashMap<PathBuf, Option<Curve>>,
}

impl Page {
    /// The page, serving the built app in `site`.
    pub fn new(site: Option<PathBuf>) -> Self {
        Self {
            site,
            curves: HashMap::new(),
        }
    }

    /// Read the curve of the ended attempt in `dir`, unless it is read already.
    fn load(&mut self, dir: &Path) -> std::io::Result<()> {
        if !self.curves.contains_key(dir) {
            let curve = Curve::read_in(dir)?;
            self.curves.insert(dir.to_path_buf(), curve);
        }
        Ok(())
    }

    /// The curve of the ended attempt in `dir`, once [`Page::load`]ed.
    fn curve(&self, dir: &Path) -> Option<&Curve> {
        self.curves.get(dir).and_then(Option::as_ref)
    }
}

/// Answer one request: a file of the built app, the state, a page of ended
/// runs, a log, a cancel, a reorder, a pause or a resume.
pub fn handle<C: Containers>(runner: &mut Runner<C>, page: &mut Page, req: &Request) -> Response {
    let answer = match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/state") => view(runner, page).and_then(|v| json(&v)),
        ("GET", "/api/ended") => {
            let number = |name: &str| req.query(name).and_then(|v| v.parse::<usize>().ok());
            let per = number("per").unwrap_or(PER_PAGE).clamp(1, MAX_PER_PAGE);
            ended(runner, number("page").unwrap_or(1).max(1), per).and_then(|p| json(&p))
        }
        // Not `/api/log`: browsers' privacy filters block that as a tracking beacon.
        ("GET", "/api/output") => log(&runner.store, req),
        ("GET", "/api/curve") => progress_curve(&runner.store, req),
        // JSON only: a cross-site page cannot post that without a CORS
        // preflight, which this server never answers, so it cannot act here.
        ("POST", _) if !is_json(req) => {
            return Response::error(415, "the page posts JSON (Content-Type: application/json)")
        }
        ("POST", "/api/cancel") => cancel(runner, req),
        ("POST", "/api/reorder") => reorder(runner, req),
        ("POST", "/api/freeze") => on_run(runner, req, Runner::freeze),
        ("POST", "/api/unfreeze") => on_run(runner, req, Runner::unfreeze),
        ("POST", "/api/pause") => on_run(runner, req, Runner::pause),
        ("POST", "/api/keep") => on_run(runner, req, Runner::keep),
        ("POST", "/api/resume") => on_run(runner, req, Runner::resume),
        ("POST", "/api/p-slot") => p_slot(runner, req),
        // A hold is the stopping mark (D10): admission starts nothing until a
        // release or the runner's next start clears it.
        ("POST", "/api/hold") => runner.store.set_stopping(true).and_then(|()| json(&true)),
        ("POST", "/api/release") => runner.release().and_then(|()| json(&true)),
        ("GET", path) if !path.starts_with("/api/") => site_file(page.site.as_deref(), path),
        _ => return Response::error(404, "no such page"),
    };
    answer.unwrap_or_else(|e| Response::error(500, &e.to_string()))
}

/// A file of the built app under `site`. A path without an extension is one
/// of the app's own routes, answered with its `index.html`; a hidden or
/// parent path component is never looked up.
fn site_file(site: Option<&Path>, path: &str) -> std::io::Result<Response> {
    let Some(site) = site else {
        return Ok(Response::error(
            404,
            "the page is not installed: set TLCQ_UI_DIR to the built ui/ (ui/dist)",
        ));
    };
    let rel = path.trim_start_matches('/');
    if rel.split('/').any(|part| part.starts_with('.')) {
        return Ok(Response::error(404, "no such page"));
    }
    let file = if Path::new(rel).extension().is_none() {
        site.join("index.html")
    } else {
        site.join(rel)
    };
    match std::fs::read(&file) {
        Ok(bytes) => Ok(Response::ok(content_type(&file), bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(Response::error(404, "no such page"))
        }
        Err(e) => Err(e),
    }
}

/// The content type of a file of the built app, by its extension.
fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// A run named by a request.
#[derive(Debug, Deserialize)]
struct Named {
    id: String,
    run: String,
}

/// A reorder's request.
#[derive(Debug, Deserialize)]
struct Moved {
    id: String,
    run: String,
    to: Move,
}

fn cancel<C: Containers>(runner: &mut Runner<C>, req: &Request) -> std::io::Result<Response> {
    let Ok(n) = serde_json::from_slice::<Named>(&req.body) else {
        return Ok(Response::error(400, "a cancel names an id and a run"));
    };
    let job = runner.cancel(&CancelRequest {
        id: n.id,
        run: Some(n.run),
        by: "ui".to_owned(),
    })?;
    json(&job)
}

/// A verb on the run a request names: freeze, unfreeze, pause or resume.
/// Answers whether it acted.
fn on_run<C: Containers>(
    runner: &mut Runner<C>,
    req: &Request,
    verb: fn(&mut Runner<C>, &str, &str) -> std::io::Result<bool>,
) -> std::io::Result<Response> {
    let Ok(n) = serde_json::from_slice::<Named>(&req.body) else {
        return Ok(Response::error(400, "the request names an id and a run"));
    };
    json(&verb(runner, &n.id, &n.run)?)
}

/// The P slot's toggle's request.
#[derive(Debug, Deserialize)]
struct Toggle {
    open: bool,
}

fn p_slot<C: Containers>(runner: &mut Runner<C>, req: &Request) -> std::io::Result<Response> {
    let Ok(t) = serde_json::from_slice::<Toggle>(&req.body) else {
        return Ok(Response::error(400, "the toggle says whether P is open"));
    };
    runner.set_p_open(t.open)?;
    json(&t.open)
}

fn reorder<C: Containers>(runner: &mut Runner<C>, req: &Request) -> std::io::Result<Response> {
    let Ok(m) = serde_json::from_slice::<Moved>(&req.body) else {
        return Ok(Response::error(
            400,
            "a reorder names an id, a run and where to",
        ));
    };
    json(&runner.reorder(&m.id, &m.run, m.to)?)
}

/// TLC's log of an attempt; only a well-formed id, run and attempt are looked up.
fn log(store: &Store, req: &Request) -> std::io::Result<Response> {
    let Some(dir) = named_attempt(store, req) else {
        return Ok(Response::error(
            400,
            "a log names an id, a run and an attempt",
        ));
    };
    match std::fs::read(dir.join("tlc.log")) {
        Ok(bytes) => Ok(Response::ok("text/plain; charset=utf-8", bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(Response::error(404, "no such log"))
        }
        Err(e) => Err(e),
    }
}

/// The directory of the attempt a request names; only a well-formed id, run
/// and attempt are looked up.
fn named_attempt(store: &Store, req: &Request) -> Option<PathBuf> {
    let id = req
        .query("id")
        .filter(|v| is(v, |c| c.is_ascii_hexdigit()))?;
    let run = req.query("run").filter(|v| {
        v.strip_prefix(['l', 'v'])
            .is_some_and(|n| is(n, |c| c.is_ascii_digit()))
    })?;
    let attempt = req.query("attempt").and_then(|v| v.parse::<u32>().ok())?;
    Some(store.attempt_dir(id, run, attempt))
}

/// An attempt's progress for the page's charts: each progress line's minutes,
/// generated, distinct and queued states, at most [`CURVE_POINTS`] of them,
/// evenly spaced, the last always kept.
fn progress_curve(store: &Store, req: &Request) -> std::io::Result<Response> {
    let Some(dir) = named_attempt(store, req) else {
        return Ok(Response::error(
            400,
            "a curve names an id, a run and an attempt",
        ));
    };
    let Some(curve) = Curve::read_in(&dir)? else {
        return Ok(Response::error(404, "no such curve"));
    };
    let every = curve.points.len().div_ceil(CURVE_POINTS).max(1);
    let last = curve.points.len().saturating_sub(1);
    let points: Vec<[f64; 4]> = curve
        .points
        .iter()
        .enumerate()
        .filter(|(i, _)| i % every == 0 || *i == last)
        .map(|(_, p)| [p.secs / 60.0, p.generated, p.distinct, p.queue])
        .collect();
    json(&points)
}

/// Whether a request's body is JSON by its content type.
fn is_json(req: &Request) -> bool {
    req.content_type.as_deref().is_some_and(|t| {
        t.split(';')
            .next()
            .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
    })
}

/// Whether `v` is non-empty and every character passes `ok`.
fn is(v: &str, ok: impl Fn(char) -> bool) -> bool {
    !v.is_empty() && v.chars().all(ok)
}

fn json(value: &impl Serialize) -> std::io::Result<Response> {
    let body = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    Ok(Response::ok("application/json", body))
}

/// What the page shows, from the runner's state and its attempts' files.
///
/// # Errors
/// If the state or a file cannot be read.
pub fn view<C: Containers>(runner: &Runner<C>, page: &mut Page) -> std::io::Result<View> {
    let store = &runner.store;
    let state = {
        let lock = store.lock()?;
        store.load(&lock)?
    };
    let mut v = View {
        slots: slots(&state, store.p_open()),
        held: store.stopping(),
        waits_for: Some(runner.waits_for(&state)),
        disk: runner.disk(),
        free_memory_mb: runner.containers.free_mb(),
        room_mb: runner.containers.room_mb(),
        shares: Some(runner.containers.shares()),
        min_mb: super::memory::MIN_MB,
        p_open: store.p_open(),
        ..View::default()
    };
    for (id, runs) in &state.runs {
        for run in runs {
            if matches!(run.phase, Phase::Ended { .. }) {
                v.ended_total += 1;
                continue;
            }
            let mut r = describe(store, &state, id, run)?;
            if r.state == "queued" {
                v.queue.push(r);
            } else {
                if matches!(r.state, "running" | "thawed" | "pausing") {
                    eta(store, page, &state, run, &mut r)?;
                }
                v.running.push(r);
            }
        }
    }
    // Positions among the queued runs only: the queue also holds live runs.
    v.queue.sort_by_key(|r| r.position);
    for (i, r) in v.queue.iter_mut().enumerate() {
        r.position = Some(i + 1);
    }
    Ok(v)
}

/// Page `page` (from 1) of the ended runs, `per` to a page, the latest end
/// first; a run whose end time is unknown comes last.
///
/// # Errors
/// If the state or a result cannot be read.
pub fn ended<C>(runner: &Runner<C>, page: usize, per: usize) -> std::io::Result<Ended> {
    let store = &runner.store;
    let state = {
        let lock = store.lock()?;
        store.load(&lock)?
    };
    let mut runs = Vec::new();
    for (id, list) in &state.runs {
        for run in list
            .iter()
            .filter(|r| matches!(r.phase, Phase::Ended { .. }))
        {
            let mut r = describe(store, &state, id, run)?;
            r.ended_at = run.ended_at;
            // A run that ended before the state kept the time: its TLC's end.
            if let (None, Some(a)) = (r.ended_at, run.attempts.last()) {
                let dir = store.attempt_dir(id, &run.name, a.number);
                r.ended_at = read_json::<AttemptResult>(&dir.join("result.json"))?
                    .and_then(|res| res.ended_at);
            }
            runs.push(r);
        }
    }
    runs.sort_by_key(|r| std::cmp::Reverse(r.ended_at));
    let total = runs.len();
    Ok(Ended {
        total,
        page,
        per,
        runs: runs
            .into_iter()
            .skip((page - 1).saturating_mul(per))
            .take(per)
            .collect(),
    })
}

/// What holds each slot, E then P.
fn slots(state: &State, p_open: bool) -> Vec<SlotView> {
    Slot::ALL
        .into_iter()
        .map(|slot| {
            let holder = state.runs.iter().find_map(|(id, runs)| {
                runs.iter().find_map(|run| {
                    let a = run.live_attempt()?;
                    let holder = match a.holds() {
                        Some(s) if s == slot && a.freeze == Freeze::Thawed => "thawed",
                        Some(s) if s == slot => "job",
                        // A whole-VM run keeps P empty for its memory (job-memory M3).
                        _ if slot == Slot::P && a.whole => "whole",
                        _ => return None,
                    };
                    Some((holder, format!("{id} {}", run.name)))
                })
            });
            SlotView {
                name: match slot {
                    Slot::E => "E",
                    Slot::P => "P",
                },
                open: slot == Slot::E || p_open,
                holder: holder.as_ref().map_or("free", |(h, _)| *h),
                run: holder.map(|(_, r)| r),
            }
        })
        .collect()
}

/// A live run's state on the page, and what a human may do with it: no pause
/// once its search has `searched` ended, as none would land.
fn run_state(run: &Run, started: bool, searched: bool) -> (&'static str, Vec<&'static str>) {
    if run.paused() {
        return ("paused", vec!["resume"]);
    }
    let Some(a) = run.live_attempt() else {
        return (if run.resume { "resuming" } else { "running" }, Vec::new());
    };
    if a.pausing {
        // Kept running only in its slot: one unpaused from frozen holds none.
        let keep = if matches!(a.freeze, Freeze::Running | Freeze::Thawed) {
            vec!["keep"]
        } else {
            Vec::new()
        };
        return ("pausing", keep);
    }
    let pause = |mut actions: Vec<&'static str>| {
        if started && !searched {
            actions.push("pause");
        }
        actions
    };
    match a.freeze {
        Freeze::Running if started => ("running", pause(vec!["freeze"])),
        Freeze::Running => ("running", Vec::new()),
        Freeze::Thawed => ("thawed", pause(vec!["unfreeze"])),
        Freeze::Frozen => ("frozen", pause(vec!["unfreeze"])),
        Freeze::Unfrozen => ("unfrozen", pause(Vec::new())),
        Freeze::Unpaused => ("pausing", Vec::new()),
    }
}

/// One run, as the page shows it.
fn describe(store: &Store, state: &State, id: &str, run: &Run) -> std::io::Result<RunView> {
    let mut r = RunView {
        id: id.to_owned(),
        run: run.name.clone(),
        spec: run.info.spec.clone(),
        engine: run.info.engine,
        configs: run.info.names.iter().map(|k| k.cfg_name.clone()).collect(),
        submitters: run.entries.iter().map(|e| e.label.to_string()).collect(),
        attempt: run.attempts.last().map(|a| a.number),
        expected: run.info.expected,
        expected_source: run.info.source,
        lanes: run.lanes.clone(),
        request_mb: run.info.memory_mb,
        ..RunView::default()
    };
    let queued_at = state
        .queue
        .iter()
        .position(|(i, n)| i == id && *n == run.name);
    r.state = match &run.phase {
        Phase::Queued => "queued",
        Phase::Running { .. } => {
            let marked = |marker| {
                run.live_attempt().is_some_and(|a| {
                    store
                        .attempt_dir(id, &run.name, a.number)
                        .join(marker)
                        .exists()
                })
            };
            let searched = marked("searched");
            let (state, actions) = run_state(run, marked("started"), searched);
            r.actions = actions;
            if let Some(a) = run.live_attempt() {
                r.since = a.frozen_at;
                r.slot = a.holds().map(|s| match s {
                    Slot::E => "E",
                    Slot::P => "P",
                });
                // A frozen job unpaused for its pause stops at once all the same.
                r.no_checkpoint = a.pausing && a.freeze != Freeze::Unpaused && searched;
                r.memory_mb = a.memory_mb;
                let scratch = store.attempt_dir(id, &run.name, a.number).join("scratch");
                r.scratch_mb = Some(super::store::dir_bytes(&scratch) >> 20);
            }
            state
        }
        Phase::Stopping { by, .. } => {
            r.cancelled_by = Some(by.clone());
            "stopping"
        }
        Phase::Ended { result } => {
            r.result = Some(result.clone());
            "ended"
        }
    };
    if r.state == "queued" {
        r.position = queued_at;
        r.movable = run.attempts.is_empty();
    }
    if let (Phase::Running { place, .. }, Some(a)) = (&run.phase, run.attempts.last()) {
        r.place = Some(place.clone());
        progress(store, id, run, a.number, &mut r)?;
        if run.paused() {
            let dir = store.attempt_dir(id, &run.name, a.number);
            r.died.clone_from(&a.died);
            r.since =
                read_json::<AttemptResult>(&dir.join("result.json"))?.and_then(|s| s.ended_at);
            r.scratch_mb =
                Some(super::store::dir_bytes(&store.checkpoint_dir(id, &run.name)) >> 20);
        }
    }
    Ok(r)
}

/// A running attempt's time, counters, rates and time left, from its files.
fn progress(
    store: &Store,
    id: &str,
    run: &Run,
    attempt: u32,
    r: &mut RunView,
) -> std::io::Result<()> {
    let dir = store.attempt_dir(id, &run.name, attempt);
    if let Some(so_far) = read_json::<AttemptResult>(&dir.join("result.json"))? {
        r.elapsed_secs = Some(so_far.duration_secs);
        r.started_at = so_far.started_at;
        r.left_secs = so_far.left_secs;
        r.lanes = so_far.lanes;
    }
    let Some(curve) = Curve::read_in(&dir)? else {
        return Ok(());
    };
    if let Some(rates) = curve.rates(WINDOW) {
        r.generated_per_min = Some(rates.generated_per_min);
        r.distinct_per_min = Some(rates.distinct_per_min);
        r.queue_share = Some(rates.queue_share);
        r.queue_growth = Some(rates.queue_growth);
        r.queue_per_min = rates.queue_per_min;
    }
    r.queue_slopes = curve.queue_slopes(WINDOW, SLOPES_SPAN);
    r.level = curve.level(WINDOW);
    r.counters = curve.counters();
    Ok(())
}

/// A count read back from a curve.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a curve's counts are whole numbers far below 2^52"
)]
fn whole(v: f64) -> u64 {
    v.max(0.0).round() as u64
}

/// A running run's ETA (D15, D16): from the curves the runner holds, a
/// previous run under the exact or configuration key, else the first-run
/// estimate over the population of other specs' runs.
fn eta(
    store: &Store,
    page: &mut Page,
    state: &State,
    run: &Run,
    r: &mut RunView,
) -> std::io::Result<()> {
    let Some(attempt) = r.attempt else {
        return Ok(());
    };
    let Some(current) = Curve::read_in(&store.attempt_dir(&r.id, &run.name, attempt))? else {
        return Ok(());
    };
    let names = &run.info.names;
    let same = |k: &Keys| names.iter().any(|n| n.same_configuration(k));
    // Ended runs of this configuration, and of other specs: each last
    // attempt's curve, read once over the page's life.
    let mut ended = Vec::new();
    for (id, runs) in &state.runs {
        for p in runs
            .iter()
            .filter(|p| matches!(p.phase, Phase::Ended { .. }))
        {
            let Some(a) = p.attempts.last() else {
                continue;
            };
            let dir = store.attempt_dir(id, &p.name, a.number);
            page.load(&dir)?;
            let Phase::Ended { result } = &p.phase else {
                continue;
            };
            if p.info.names.iter().any(same) {
                ended.push((dir, true, result.duration_secs));
            } else if p.info.spec != run.info.spec && p.info.engine == run.info.engine {
                // Estimates never cross engines (tlx-engine E7).
                ended.push((dir, false, result.duration_secs));
            }
        }
    }
    let complete = |dir| page.curve(dir).filter(|c| c.complete);
    let previous = ended
        .iter()
        .rev()
        .filter(|(_, same, _)| *same)
        .find_map(|(dir, _, secs)| complete(dir).map(|curve| (curve, *secs)));
    // The last known time: that run's whole duration.
    r.last_secs = previous.map(|(_, secs)| secs);
    r.depth = current.depth(previous.map(|(curve, _)| curve));
    let previous = previous.map(|(curve, _)| Previous {
        curve,
        by_name: false,
    });
    let others = ended
        .iter()
        .filter(|(_, same, _)| !same)
        .filter_map(|(dir, _, _)| complete(dir));
    if let Some(e) = estimate(&current, previous, &Population::new(others)) {
        let last = current.points.last().map_or(0.0, |p| p.secs);
        r.eta_secs = Some(whole(e.end_secs - last));
        r.eta_source = Some(e.source);
    }
    Ok(())
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;

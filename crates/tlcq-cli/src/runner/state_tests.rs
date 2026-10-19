//! Tests of the runner's state and its preflight.

use std::path::PathBuf;

use super::*;
use crate::entry::{Key, Label};
use crate::history::Keys;

pub(crate) fn key(cfg: &str) -> Key {
    Key {
        worktree: PathBuf::from("/w1"),
        model_dir: PathBuf::from("tla/m"),
        spec: "M".to_owned(),
        cfg_name: cfg.to_owned(),
    }
}

pub(crate) fn entry(cfg: &str) -> Entry {
    Entry {
        key: key(cfg),
        label: Label {
            project: "p".to_owned(),
            worktree: PathBuf::from("/w1"),
            branch: None,
            agent: None,
        },
    }
}

pub(crate) fn info() -> JobInfo {
    JobInfo {
        entries: Vec::new(),
        run: None,
        spec: "M".to_owned(),
        engine: crate::engine::Engine::Tlc,
        tlc_opts: Vec::new(),
        names: vec![Keys {
            engine: crate::engine::Engine::Tlc,
            spec_hash: "s".to_owned(),
            cfg_hash: "c".to_owned(),
            options_hash: "o".to_owned(),
            spec: "M".to_owned(),
            cfg_name: "A.cfg".to_owned(),
        }],
        expected: None,
        source: None,
        timeout_secs: None,
        local: false,
        vm_on_timeout: false,
        memory_mb: None,
        laptop_secs: 0,
    }
}

pub(crate) fn run(name: &str, phase: Phase, cfg: &str) -> Run {
    Run {
        name: name.to_owned(),
        phase,
        entries: vec![entry(cfg)],
        info: info(),
        attempts: Vec::new(),
        last_attempt: 0,
        undone: 0,
        resume: false,
        duration_secs: 0,
        lanes: Vec::new(),
        ended_at: None,
    }
}

pub(crate) fn running() -> Phase {
    Phase::Running {
        place: "vm E".to_owned(),
        elapsed_secs: 90,
        counters: None,
    }
}

fn request(ids: &[&str], keys: &[&str], tokens: &[&str]) -> PreflightRequest {
    PreflightRequest {
        versions: crate::engine::test_versions("J"),
        ids: ids.iter().map(|s| (*s).to_owned()).collect(),
        keys: keys.iter().map(|k| key(k)).collect(),
        tokens: tokens.iter().map(|s| (*s).to_owned()).collect(),
        runs: Vec::new(),
    }
}

#[test]
fn the_state_of_an_id_prefers_a_live_run_then_the_cache() {
    let mut s = State::default();
    assert_eq!(s.vm_job("a").state, VmState::Unknown);
    let ok = State::outcome_only(Outcome::Ok);
    s.runs.insert(
        "a".to_owned(),
        vec![run("v1", Phase::Ended { result: ok.clone() }, "A.cfg")],
    );
    assert_eq!(s.vm_job("a").state, VmState::Ended { result: ok.clone() });
    s.cache.insert("a".to_owned(), ok.clone());
    assert_eq!(s.vm_job("a").state, VmState::Cached { result: ok });
    s.runs
        .get_mut("a")
        .unwrap()
        .push(run("v2", running(), "A.cfg"));
    let job = s.vm_job("a");
    assert_eq!(job.state, VmState::Running);
    assert_eq!(
        (job.run.as_deref(), job.elapsed_secs),
        (Some("v2"), Some(90))
    );
    s.runs.get_mut("a").unwrap().push(run(
        "v3",
        Phase::Stopping {
            by: "ui".to_owned(),
            replaced_by: None,
        },
        "A.cfg",
    ));
    assert_eq!(s.vm_job("a").state, VmState::Stopping);
    s.runs
        .get_mut("a")
        .unwrap()
        .push(run("v4", Phase::Queued, "A.cfg"));
    assert_eq!(s.vm_job("a").state, VmState::Queued);
}

#[test]
fn the_preflight_tombstones_tokens_no_commit_applied() {
    let mut s = State::default();
    let applied = CommitAnswer::Applied {
        jobs: BTreeMap::new(),
    };
    s.tokens.insert("t1".to_owned(), applied.clone());
    let reply = s.preflight(
        &request(&[], &[], &["t1", "t2"]),
        &crate::engine::test_versions("J"),
    );
    assert_eq!(reply.tokens.get("t1"), Some(&applied));
    assert_eq!(reply.tokens.get("t2"), Some(&CommitAnswer::NotApplied));
    assert_eq!(s.tokens.get("t2"), Some(&CommitAnswer::NotApplied));
    assert!(reply.jars_match);
    assert!(
        !s.preflight(&request(&[], &[], &[]), &crate::engine::test_versions("K"))
            .jars_match
    );
}

#[test]
fn the_preflight_reports_live_older_versions_under_the_keys() {
    let mut s = State::default();
    s.runs
        .insert("old".to_owned(), vec![run("v1", running(), "A.cfg")]);
    s.runs
        .insert("queued".to_owned(), vec![run("v1", Phase::Queued, "A.cfg")]);
    s.runs
        .insert("other".to_owned(), vec![run("v1", running(), "B.cfg")]);
    s.runs.insert(
        "done".to_owned(),
        vec![run(
            "v1",
            Phase::Ended {
                result: State::outcome_only(Outcome::Ok),
            },
            "A.cfg",
        )],
    );
    let reply = s.preflight(
        &request(&["old"], &["A.cfg"], &[]),
        &crate::engine::test_versions("J"),
    );
    let older: Vec<(&str, bool, u64)> = reply
        .older
        .iter()
        .map(|o| (o.id.as_str(), o.running, o.elapsed_secs))
        .collect();
    assert_eq!(older, [("old", true, 90), ("queued", false, 0)]);
    assert_eq!(
        reply.jobs.get("old").map(|j| &j.state),
        Some(&VmState::Running)
    );
}

#[test]
fn ending_a_run_caches_a_verdict_and_leaves_the_queue() {
    let mut s = State::default();
    s.runs
        .insert("a".to_owned(), vec![run("v1", Phase::Queued, "A.cfg")]);
    s.queue.push(("a".to_owned(), "v1".to_owned()));
    s.end("a", "v1", State::outcome_only(Outcome::Unexpected));
    assert_eq!(s.queue, Vec::<(String, String)>::new());
    assert!(s.cache.contains_key("a"));
    s.runs
        .get_mut("a")
        .unwrap()
        .push(run("v2", Phase::Queued, "A.cfg"));
    s.end("a", "v2", State::outcome_only(Outcome::Ok));
    assert_eq!(
        s.cache.get("a").map(|r| &r.outcome),
        Some(&Outcome::Unexpected)
    );
    s.end("b", "v1", State::outcome_only(Outcome::Timeout));
    assert!(!s.cache.contains_key("b"));
}

#[test]
fn a_run_s_hold_follows_its_attempt() {
    use crate::proto::Held;
    let with = |freeze, pausing| {
        let mut r = run("v1", running(), "A.cfg");
        r.attempts.push(Attempt {
            freeze,
            pausing,
            frozen_at: Some(7),
            ..Attempt::new(1, Slot::E, false)
        });
        r.held()
    };
    let frozen = |spare| {
        Some(Held::Frozen {
            since: Some(7),
            spare,
        })
    };
    assert_eq!(with(Freeze::Running, false), None);
    assert_eq!(with(Freeze::Unpaused, false), None);
    assert_eq!(with(Freeze::Frozen, false), frozen(false));
    assert_eq!(with(Freeze::Unfrozen, false), Some(Held::Waiting));
    assert_eq!(with(Freeze::Thawed, false), frozen(true));
    assert_eq!(with(Freeze::Unpaused, true), Some(Held::Pausing));
    // Paused: its last attempt ended for a pause, no resume queued; none
    // without an attempt.
    let mut r = run("v1", running(), "A.cfg");
    assert_eq!(r.held(), None);
    r.attempts.push(Attempt {
        ended: true,
        paused: true,
        ..Attempt::new(1, Slot::P, false)
    });
    assert_eq!(
        r.held(),
        Some(Held::Paused {
            at: None,
            died: None,
            checkpoint_mb: None
        })
    );
    r.resume = true;
    // Resumed, waiting first for a slot.
    assert_eq!(r.held(), Some(Held::Waiting));
    assert!(!r.paused());
}

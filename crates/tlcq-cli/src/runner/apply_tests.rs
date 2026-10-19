//! Tests of applying a commit to the runner's state.

use super::*;
use crate::commit::{CommitJob, CommitRoute};
use crate::outcome::Outcome;
use crate::runner::state::tests::{entry, info, key, run, running};
use crate::runner::state::{Attempt, Slot};

fn job(id: &str, cfg: &str) -> CommitJob {
    let mut info = info();
    info.entries = vec![entry(cfg)];
    CommitJob {
        id: id.to_owned(),
        info,
        route: CommitRoute::Queue,
        tar_len: 0,
    }
}

fn commit(token: &str, jobs: Vec<CommitJob>) -> CommitRequest {
    CommitRequest {
        versions: crate::engine::test_versions("J"),
        token: token.to_owned(),
        pending: Vec::new(),
        jobs,
        supersede: Vec::new(),
    }
}

fn answers(reply: &CommitReply) -> &BTreeMap<String, JobAnswer> {
    match reply {
        CommitReply::Applied { jobs, .. } => jobs,
        other => panic!("not applied: {other:?}"),
    }
}

fn queued(run: &str) -> JobAnswer {
    JobAnswer::Queued {
        run: run.to_owned(),
    }
}

fn supersede(old: &str, cfg: &str, attached: bool) -> SupersedeOrder {
    SupersedeOrder {
        old: old.to_owned(),
        key: key(cfg),
        by: "new".to_owned(),
        label: "agent".to_owned(),
        attached,
    }
}

#[test]
fn a_new_job_is_queued_as_its_next_numbered_run() {
    let mut s = State::default();
    s.numbered.insert("a".to_owned(), 2);
    let (reply, effects) = s.apply_commit(&commit("t1", vec![job("a", "A.cfg")]));
    assert_eq!(answers(&reply).get("a"), Some(&queued("v3")));
    assert_eq!(effects, Effects::default());
    assert_eq!(s.queue, [("a".to_owned(), "v3".to_owned())]);
    assert_eq!(s.latest("a").map(|r| r.entries.len()), Some(1));
    let reached = TokenAnswer::Reached {
        run: "v3".to_owned(),
    };
    assert_eq!(
        s.tokens.get("t1"),
        Some(&CommitAnswer::Applied {
            jobs: BTreeMap::from([("a".to_owned(), reached)])
        })
    );
}

#[test]
fn a_moved_run_keeps_its_laptop_name() {
    let mut s = State::default();
    let mut moved = job("a", "A.cfg");
    moved.info.run = Some("l1".to_owned());
    let (reply, _) = s.apply_commit(&commit("t1", vec![moved]));
    assert_eq!(answers(&reply).get("a"), Some(&queued("l1")));
    assert!(!s.numbered.contains_key("a"));
}

#[test]
fn a_live_run_is_attached_to_and_a_cached_id_answered() {
    let mut s = State::default();
    s.runs
        .insert("a".to_owned(), vec![run("v1", running(), "A.cfg")]);
    let ok = State::outcome_only(Outcome::Ok);
    s.cache.insert("b".to_owned(), ok.clone());
    let jobs = vec![job("a", "A.cfg"), job("a", "B.cfg"), job("b", "A.cfg")];
    let (reply, _) = s.apply_commit(&commit("t1", jobs));
    let jobs = answers(&reply);
    assert_eq!(
        jobs.get("a"),
        Some(&JobAnswer::Attached {
            run: "v1".to_owned()
        })
    );
    assert_eq!(jobs.get("b"), Some(&JobAnswer::Cached { result: ok }));
    assert_eq!(
        s.latest("a").unwrap().entries,
        [entry("A.cfg"), entry("B.cfg")]
    );
    assert!(!s.runs.contains_key("b"));
    assert_eq!(s.queue, Vec::new());
}

#[test]
fn a_token_applies_once_and_a_tombstone_applies_nothing() {
    let mut s = State::default();
    s.tokens.insert("dead".to_owned(), CommitAnswer::NotApplied);
    let (reply, _) = s.apply_commit(&commit("dead", vec![job("a", "A.cfg")]));
    assert_eq!(reply, CommitReply::NotApplied);
    assert!(s.runs.is_empty());
    s.apply_commit(&commit("t1", vec![job("a", "A.cfg")]));
    let (again, _) = s.apply_commit(&commit("t1", vec![job("a", "A.cfg")]));
    assert_eq!(
        answers(&again).get("a"),
        Some(&JobAnswer::Attached {
            run: "v1".to_owned()
        })
    );
    assert_eq!(s.runs.get("a").map(Vec::len), Some(1));
}

#[test]
fn a_pending_entry_joins_its_own_run_only() {
    let mut s = State::default();
    let ended = Phase::Ended {
        result: State::outcome_only(Outcome::Ok),
    };
    s.runs.insert(
        "a".to_owned(),
        vec![run("v1", ended, "A.cfg"), run("v2", running(), "A.cfg")],
    );
    s.apply_commit(&commit("t0", vec![job("b", "A.cfg")]));
    s.tokens.insert("dead".to_owned(), CommitAnswer::NotApplied);
    let flush = |id: &str, run: RunRef| PendingFlush {
        id: id.to_owned(),
        run,
        entry: entry("P.cfg"),
    };
    let mut req = commit("t1", Vec::new());
    req.pending = vec![
        flush("a", RunRef::Run("v2".to_owned())),
        flush("a", RunRef::Run("v1".to_owned())),
        flush("b", RunRef::Token("t0".to_owned())),
        flush("b", RunRef::Token("dead".to_owned())),
        flush("c", RunRef::Token("t0".to_owned())),
    ];
    let (reply, _) = s.apply_commit(&req);
    let CommitReply::Applied { dropped, .. } = reply else {
        panic!("not applied");
    };
    let dropped: Vec<(&str, &RunRef)> = dropped.iter().map(|p| (p.id.as_str(), &p.run)).collect();
    assert_eq!(
        dropped,
        [
            ("a", &RunRef::Run("v1".to_owned())),
            ("b", &RunRef::Token("dead".to_owned())),
            ("c", &RunRef::Token("t0".to_owned())),
        ]
    );
    assert!(s.latest("a").unwrap().entries.contains(&entry("P.cfg")));
    assert!(s.latest("b").unwrap().entries.contains(&entry("P.cfg")));
}

#[test]
fn a_supersede_cancels_an_old_version_left_without_entries() {
    let mut s = State::default();
    s.runs
        .insert("q".to_owned(), vec![run("v1", Phase::Queued, "A.cfg")]);
    s.queue.push(("q".to_owned(), "v1".to_owned()));
    let mut r = run("v1", running(), "A.cfg");
    r.attempts.push(Attempt::new(2, Slot::E, false));
    s.runs.insert("r".to_owned(), vec![r]);
    let mut held = run("v1", running(), "A.cfg");
    held.resume = true;
    s.runs.insert("h".to_owned(), vec![held]);
    let mut shared = run("v1", running(), "A.cfg");
    shared.entries.push(entry("B.cfg"));
    s.runs.insert("s".to_owned(), vec![shared]);
    s.runs
        .insert("t".to_owned(), vec![run("v1", running(), "A.cfg")]);
    let mut req = commit("t1", Vec::new());
    req.supersede = vec![
        supersede("q", "A.cfg", false),
        supersede("r", "A.cfg", false),
        supersede("h", "A.cfg", false),
        supersede("s", "A.cfg", false),
        supersede("t", "A.cfg", true),
        supersede("gone", "A.cfg", false),
    ];
    let (_, effects) = s.apply_commit(&req);
    let marker = CancelMarker {
        by: "agent".to_owned(),
        replaced_by: Some("new".to_owned()),
    };
    let stop = Stop {
        id: "r".to_owned(),
        run: "v1".to_owned(),
        attempt: 2,
        marker,
    };
    assert_eq!(effects.stop, [stop]);
    let cancelled = Outcome::Cancelled {
        by: "agent".to_owned(),
        replaced_by: Some("new".to_owned()),
    };
    for id in ["q", "h"] {
        let r = s.latest(id).unwrap();
        assert!(matches!(&r.phase, Phase::Ended { result } if result.outcome == cancelled));
        assert!(!r.resume);
    }
    assert_eq!(s.queue, Vec::new());
    assert!(matches!(
        s.latest("r").unwrap().phase,
        Phase::Stopping { .. }
    ));
    assert_eq!(s.latest("s").unwrap().entries, [entry("B.cfg")]);
    let t = s.latest("t").unwrap();
    assert!(t.live() && t.entries.is_empty());
}

#[test]
fn a_token_records_what_each_answer_reached() {
    let ok = State::outcome_only(Outcome::Ok);
    let run = || "v1".to_owned();
    assert_eq!(
        token_answer(&JobAnswer::Started { run: run() }),
        TokenAnswer::Reached { run: run() }
    );
    assert_eq!(token_answer(&JobAnswer::Refused), TokenAnswer::NotApplied);
    assert_eq!(
        token_answer(&JobAnswer::Cached { result: ok.clone() }),
        TokenAnswer::Cached { result: ok.clone() }
    );
    let jobs = BTreeMap::from([
        ("a".to_owned(), TokenAnswer::Cached { result: ok.clone() }),
        ("b".to_owned(), TokenAnswer::NotApplied),
    ]);
    let replayed = replay(&jobs);
    let answers = answers(&replayed);
    assert_eq!(answers.get("a"), Some(&JobAnswer::Cached { result: ok }));
    assert_eq!(answers.get("b"), Some(&JobAnswer::Refused));
}

#[test]
fn a_now_or_refuse_job_that_did_not_start_is_refused_and_removed() {
    let mut s = State::default();
    let mut jobs = vec![job("a", "A.cfg"), job("b", "B.cfg"), job("c", "C.cfg")];
    jobs[0].route = CommitRoute::NowOrRefuse;
    jobs[1].route = CommitRoute::NowOrRefuse;
    let req = commit("t1", jobs);
    let (mut reply, _) = s.apply_commit(&req);
    s.runs.get_mut("a").unwrap()[0].phase = running();
    s.answer_starts(&req, &mut reply);
    let jobs = answers(&reply);
    assert_eq!(
        jobs.get("a"),
        Some(&JobAnswer::Started {
            run: "v1".to_owned()
        })
    );
    assert_eq!(jobs.get("b"), Some(&JobAnswer::Refused));
    assert_eq!(jobs.get("c"), Some(&queued("v1")));
    assert!(!s.runs.contains_key("b"));
    assert_eq!(
        s.queue.iter().map(|(i, _)| i.as_str()).collect::<Vec<_>>(),
        ["a", "c"]
    );
    let Some(CommitAnswer::Applied { jobs: tokens }) = s.tokens.get("t1") else {
        panic!("no token");
    };
    assert_eq!(tokens.get("b"), Some(&TokenAnswer::NotApplied));
    let mut not_applied = CommitReply::NotApplied;
    s.answer_starts(&req, &mut not_applied);
    assert_eq!(not_applied, CommitReply::NotApplied);
}

#[test]
fn a_moved_run_whose_name_the_id_already_used_gets_a_vm_name() {
    let mut s = State::default();
    let ended = Phase::Ended {
        result: State::outcome_only(Outcome::Timeout),
    };
    s.runs
        .insert("a".to_owned(), vec![run("l1", ended, "A.cfg")]);
    let mut moved = job("a", "A.cfg");
    moved.info.run = Some("l1".to_owned());
    let (reply, _) = s.apply_commit(&commit("t1", vec![moved]));
    assert_eq!(answers(&reply).get("a"), Some(&queued("v1")));
    assert_eq!(s.queue, [("a".to_owned(), "v1".to_owned())]);
}

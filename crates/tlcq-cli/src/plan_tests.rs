//! Tests of the placement plan, one per rule of design D6 and the job-submit spec.

use std::path::PathBuf;

use super::*;
use crate::batch::Named;
use crate::config::{Expect, JobOptions};
use crate::history::Keys;
use crate::job::{Contents, Limits};
use crate::outcome::Outcome;
use crate::proto::{VmJob, VmOlder};
use crate::store::Pending;

fn job(id: &str, names: &[&str]) -> Job {
    Job {
        id: id.to_owned(),
        engine: crate::engine::Engine::Tlc,
        manifest: String::new(),
        contents: Contents::new(Vec::new(), Limits::default()).unwrap(),
        names: names
            .iter()
            .map(|n| Named {
                name: (*n).to_owned(),
                keys: Keys {
                    engine: crate::engine::Engine::Tlc,
                    spec_hash: format!("s-{id}"),
                    cfg_hash: format!("c-{id}"),
                    options_hash: "o".to_owned(),
                    spec: "M".to_owned(),
                    cfg_name: (*n).to_owned(),
                },
                options: JobOptions::default(),
            })
            .collect(),
    }
}

fn with_expect(mut job: Job, e: Expect) -> Job {
    for n in &mut job.names {
        n.options.expect = Some(e);
    }
    job
}

fn label(worktree: &str) -> Label {
    Label {
        project: "p".to_owned(),
        worktree: PathBuf::from(worktree),
        branch: None,
        agent: None,
    }
}

fn cmd(jobs: &[Job]) -> Command<'_> {
    Command {
        jobs,
        worktree: PathBuf::from("/w1"),
        model_dir: PathBuf::from("tla/m"),
        label: label("/w1"),
        replace: false,
        local: false,
        skip_max: false,
    }
}

fn key(worktree: &str, cfg: &str) -> Key {
    Key {
        worktree: PathBuf::from(worktree),
        model_dir: PathBuf::from("tla/m"),
        spec: "M".to_owned(),
        cfg_name: cfg.to_owned(),
    }
}

fn entry(worktree: &str, cfg: &str) -> Entry {
    Entry {
        key: key(worktree, cfg),
        label: label(worktree),
    }
}

fn result(outcome: Outcome) -> JobResult {
    JobResult {
        outcome,
        exit_status: Some(0),
        distinct: None,
        generated: None,
        depth: None,
        place: "vm".to_owned(),
        duration_secs: 1,
        lanes: Vec::new(),
        errors: Vec::new(),
        note: None,
    }
}

fn vm() -> PreflightReply {
    PreflightReply {
        jars_match: true,
        tlx_match: Some(true),
        ..PreflightReply::default()
    }
}

fn vm_job(reply: &mut PreflightReply, id: &str, state: VmState) {
    reply.jobs.insert(
        id.to_owned(),
        VmJob {
            state,
            run: Some("v1".to_owned()),
            entries: Vec::new(),
            place: None,
            elapsed_secs: None,
            counters: None,
            lanes: Vec::new(),
            flags: crate::proto::Flags::default(),
            curve: None,
            position: None,
            left_secs: None,
            cancelled: None,
            started_at: None,
            expected: None,
            waits_for: None,
            held: None,
        },
    );
}

fn laptop_job(laptop: &mut Laptop, id: &str, cfg: &str, running: bool) {
    let mut r = JobRecord::new(id, State::Laptop);
    r.entries.push(entry("/w1", cfg));
    laptop.records.insert(id.to_owned(), r);
    laptop.locked.insert(id.to_owned());
    if running {
        laptop.running.insert(id.to_owned());
    }
}

fn kinds(plan: &Plan) -> Vec<(&Kind, Route)> {
    plan.jobs.iter().map(|j| (&j.kind, j.route)).collect()
}

#[test]
fn a_jar_mismatch_refuses_the_command() {
    let jobs = [job("a", &["A.cfg"])];
    let mut reply = vm();
    reply.jars_match = false;
    let p = plan(
        &cmd(&jobs),
        &Laptop::default(),
        Some(&reply),
        &History::default(),
    );
    assert_eq!(p.refusals, [Refusal::JarMismatch]);
}

#[test]
fn only_the_engines_a_command_uses_must_match() {
    let tlc = job("a", &["A.cfg"]);
    let tlx = Job {
        engine: crate::engine::Engine::Tlx,
        ..job("b", &["B.cfg"])
    };
    let refusals = |jobs: &[Job], jars: bool, tlx: bool| {
        let mut reply = vm();
        reply.jars_match = jars;
        reply.tlx_match = Some(tlx);
        plan(
            &cmd(jobs),
            &Laptop::default(),
            Some(&reply),
            &History::default(),
        )
        .refusals
    };
    // A stale tlx refuses a tlx-only and a mixed command, not a TLC-only one.
    let both = [tlc.clone(), tlx.clone()];
    let (tlc, tlx) = (std::slice::from_ref(&tlc), std::slice::from_ref(&tlx));
    assert_eq!(refusals(tlx, true, false), [Refusal::TlxMismatch]);
    assert_eq!(refusals(&both, true, false), [Refusal::TlxMismatch]);
    assert_eq!(refusals(tlc, true, false), []);
    // Stale jars do not refuse a tlx-only command.
    assert_eq!(refusals(tlx, false, true), []);
    assert_eq!(refusals(tlc, false, true), [Refusal::JarMismatch]);
    // A runner from before engines answers no tlx match at all.
    let mut old = vm();
    old.tlx_match = None;
    let p = plan(
        &cmd(tlc),
        &Laptop::default(),
        Some(&old),
        &History::default(),
    );
    assert_eq!(p.refusals, [Refusal::RunnerOutdated]);
}

#[test]
fn cache_hits_from_either_cache_and_no_older_version_matters() {
    let jobs = [job("a", &["A.cfg"]), job("b", &["B.cfg"])];
    let mut laptop = Laptop::default();
    laptop.cache.insert("a".to_owned(), result(Outcome::Ok));
    laptop_job(&mut laptop, "old", "A.cfg", true);
    let mut reply = vm();
    vm_job(
        &mut reply,
        "b",
        VmState::Cached {
            result: result(Outcome::Unexpected),
        },
    );
    let p = plan(&cmd(&jobs), &laptop, Some(&reply), &History::default());
    assert_eq!(
        kinds(&p),
        [
            (&Kind::CacheHit(result(Outcome::Ok)), Route::None),
            (&Kind::CacheHit(result(Outcome::Unexpected)), Route::None)
        ]
    );
    assert_eq!(p.refusals, []);
    assert_eq!(p.supersede, []);
}

#[test]
fn attaches_to_the_vm_and_to_the_laptop_but_not_to_a_stopping_job() {
    let jobs = [
        job("a", &["A.cfg"]),
        job("b", &["B.cfg"]),
        job("c", &["C.cfg"]),
    ];
    let mut reply = vm();
    vm_job(&mut reply, "a", VmState::Running);
    vm_job(&mut reply, "c", VmState::Stopping);
    let mut laptop = Laptop::default();
    let mut b = JobRecord::new("b", State::Laptop);
    b.entries.push(entry("/w2", "B.cfg"));
    laptop.records.insert("b".to_owned(), b);
    laptop.locked.insert("b".to_owned());
    laptop.free_slots = 0;
    let p = plan(&cmd(&jobs), &laptop, Some(&reply), &History::default());
    assert_eq!(
        kinds(&p),
        [
            (&Kind::AttachVm, Route::VmAttach),
            (&Kind::AttachLaptop, Route::LaptopAttach),
            (&Kind::New, Route::VmQueue)
        ]
    );
}

#[test]
fn offline_attaches_name_the_run_or_the_token() {
    let jobs = [job("a", &["A.cfg"]), job("b", &["B.cfg"])];
    let mut laptop = Laptop::default();
    let mut a = JobRecord::new("a", State::Vm);
    a.run = Some("v3".to_owned());
    let mut b = JobRecord::new("b", State::Sent);
    b.token = Some("t".to_owned());
    laptop.records.insert("a".to_owned(), a);
    laptop.records.insert("b".to_owned(), b);
    let p = plan(&cmd(&jobs), &laptop, None, &History::default());
    assert_eq!(
        kinds(&p),
        [
            (
                &Kind::AttachOffline(RunRef::Run("v3".to_owned())),
                Route::Pending
            ),
            (
                &Kind::AttachOffline(RunRef::Token("t".to_owned())),
                Route::Pending
            )
        ]
    );
}

#[test]
fn a_running_older_version_refuses_unless_replace_and_a_queued_one_is_superseded() {
    let jobs = [job("new", &["A.cfg"])];
    let mut laptop = Laptop::default();
    laptop_job(&mut laptop, "old", "A.cfg", true);
    let p = plan(&cmd(&jobs), &laptop, Some(&vm()), &History::default());
    assert_eq!(
        p.refusals,
        [Refusal::OlderRunning {
            id: "new".to_owned(),
            old: "old".to_owned()
        }]
    );
    let mut replace = cmd(&jobs);
    replace.replace = true;
    let p = plan(&replace, &laptop, Some(&vm()), &History::default());
    assert_eq!(p.refusals, []);
    assert_eq!(
        p.supersede,
        [Supersede {
            old: "old".to_owned(),
            key: key("/w1", "A.cfg"),
            on_laptop: true,
            by: "new".to_owned(),
            attached: false
        }]
    );
    let mut queued = Laptop::default();
    laptop_job(&mut queued, "old", "A.cfg", false);
    let p = plan(&cmd(&jobs), &queued, Some(&vm()), &History::default());
    assert_eq!(p.refusals, []);
    assert_eq!(p.supersede.len(), 1);
}

#[test]
fn older_versions_on_the_vm_come_from_the_preflight() {
    let jobs = [job("new", &["A.cfg"])];
    let mut reply = vm();
    reply.older.push(VmOlder {
        id: "old".to_owned(),
        key: key("/w1", "A.cfg"),
        running: false,
        place: "vm".to_owned(),
        elapsed_secs: 60,
        counters: None,
    });
    reply.older.push(VmOlder {
        id: "other".to_owned(),
        key: key("/w2", "A.cfg"),
        running: true,
        place: "vm".to_owned(),
        elapsed_secs: 60,
        counters: None,
    });
    let p = plan(
        &cmd(&jobs),
        &Laptop::default(),
        Some(&reply),
        &History::default(),
    );
    assert_eq!(p.refusals, []);
    assert_eq!(
        p.supersede,
        [Supersede {
            old: "old".to_owned(),
            key: key("/w1", "A.cfg"),
            on_laptop: false,
            by: "new".to_owned(),
            attached: false
        }]
    );
}

#[test]
fn an_attach_counts_as_a_run() {
    let jobs = [job("a", &["A.cfg"])];
    let mut reply = vm();
    vm_job(&mut reply, "a", VmState::Running);
    reply.older.push(VmOlder {
        id: "b".to_owned(),
        key: key("/w1", "A.cfg"),
        running: true,
        place: "vm".to_owned(),
        elapsed_secs: 60,
        counters: None,
    });
    let p = plan(
        &cmd(&jobs),
        &Laptop::default(),
        Some(&reply),
        &History::default(),
    );
    assert_eq!(p.refusals.len(), 1);
}

#[test]
fn pending_entries_count_in_the_older_version_scan() {
    let jobs = [job("new", &["A.cfg"])];
    let mut laptop = Laptop::default();
    let mut old = JobRecord::new("old", State::Vm);
    old.run = Some("v1".to_owned());
    old.pending.push(Pending {
        entry: entry("/w1", "A.cfg"),
        run: RunRef::Run("v1".to_owned()),
    });
    laptop.records.insert("old".to_owned(), old);
    let mut reply = vm();
    vm_job(&mut reply, "old", VmState::Queued);
    let p = plan(&cmd(&jobs), &laptop, Some(&reply), &History::default());
    assert_eq!(p.supersede.len(), 1);
    vm_job(&mut reply, "old", VmState::Running);
    let p = plan(&cmd(&jobs), &laptop, Some(&reply), &History::default());
    assert_eq!(p.refusals.len(), 1);
    vm_job(&mut reply, "old", VmState::Stopping);
    let p = plan(&cmd(&jobs), &laptop, Some(&reply), &History::default());
    assert_eq!((p.refusals.len(), p.supersede.len()), (0, 0));
}

#[test]
fn offline_an_older_version_that_may_be_on_the_vm_counts_as_running() {
    let jobs = [job("new", &["A.cfg"])];
    let mut laptop = Laptop::default();
    let mut old = JobRecord::new("old", State::Sent);
    old.token = Some("t".to_owned());
    old.mirror.push(entry("/w1", "A.cfg"));
    laptop.records.insert("old".to_owned(), old);
    let p = plan(&cmd(&jobs), &laptop, None, &History::default());
    let unreachable = Refusal::ReplaceUnreachable {
        id: "new".to_owned(),
        old: "old".to_owned(),
    };
    assert_eq!(p.refusals, [unreachable]);
    let mut replace = cmd(&jobs);
    replace.replace = true;
    let p = plan(&replace, &laptop, None, &History::default());
    assert_eq!(
        p.refusals,
        [Refusal::ReplaceUnreachable {
            id: "new".to_owned(),
            old: "old".to_owned()
        }]
    );
}

#[test]
fn offline_long_jobs_are_refused_unless_local() {
    let jobs = [
        with_expect(job("a", &["A.cfg"]), Expect::Slow),
        job("b", &["B.cfg"]),
    ];
    let p = plan(&cmd(&jobs), &Laptop::default(), None, &History::default());
    assert_eq!(p.refusals, [Refusal::LongOffline { id: "a".to_owned() }]);
    let mut local = cmd(&jobs);
    local.local = true;
    let p = plan(&local, &Laptop::default(), None, &History::default());
    assert_eq!(p.refusals, []);
    assert_eq!(
        kinds(&p),
        [(&Kind::New, Route::Laptop), (&Kind::New, Route::Laptop)]
    );
}

#[test]
fn a_memory_request_runs_only_on_the_vm() {
    let mut asking = with_expect(job("a", &["A.cfg"]), Expect::Fast);
    asking.names[0].options.memory_mb = Some(65_536);
    let jobs = [asking, with_expect(job("b", &["B.cfg"]), Expect::Fast)];
    // A short job asking for memory is queued on the VM, not run in a laptop slot.
    let laptop = Laptop {
        free_slots: 2,
        ..Laptop::default()
    };
    let p = plan(&cmd(&jobs), &laptop, Some(&vm()), &History::default());
    assert_eq!(p.refusals, []);
    assert_eq!(
        kinds(&p),
        [
            (&Kind::New, Route::VmQueue),
            (&Kind::New, Route::VmNowOrRefuse)
        ]
    );
    // `--local`, or the VM unreachable, would run it here: refused.
    let mut local = cmd(&jobs);
    local.local = true;
    let refused = [Refusal::MemoryOnLaptop { id: "a".to_owned() }];
    assert_eq!(
        plan(&local, &laptop, Some(&vm()), &History::default()).refusals,
        refused
    );
    assert_eq!(
        plan(&cmd(&jobs), &laptop, None, &History::default()).refusals,
        refused
    );
}

#[test]
fn slots_go_to_unknown_jobs_first_then_short_ones() {
    let jobs = [
        with_expect(job("s1", &["S1.cfg"]), Expect::Fast),
        job("u1", &["U1.cfg"]),
        with_expect(job("l1", &["L1.cfg"]), Expect::Max),
        with_expect(job("s2", &["S2.cfg"]), Expect::Fast),
    ];
    let laptop = Laptop {
        free_slots: 2,
        ..Laptop::default()
    };
    let p = plan(&cmd(&jobs), &laptop, Some(&vm()), &History::default());
    let routes: Vec<Route> = p.jobs.iter().map(|j| j.route).collect();
    assert_eq!(
        routes,
        [
            Route::VmNowOrRefuse,
            Route::Laptop,
            Route::VmQueue,
            Route::VmNowOrQueue
        ]
    );
    let none = plan(
        &cmd(&jobs[1..2]),
        &Laptop::default(),
        Some(&vm()),
        &History::default(),
    );
    assert_eq!(none.jobs.first().map(|j| j.route), Some(Route::VmQueue));
}

#[test]
fn a_merged_job_takes_its_longest_name_and_warns_on_a_contradicted_suffix() {
    let jobs = [job("a", &["A_max.cfg", "B.cfg"]), job("c", &["C_fast.cfg"])];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let c = jobs[1].names[0].keys.clone();
    History::append(
        &path,
        &crate::history::Record {
            id: "c".to_owned(),
            end: "v1".to_owned(),
            names: vec![c],
            expected: Some(Expected::Long),
            lower_bound: false,
            duration_secs: 600,
            outcome: "ok".to_owned(),
            place: "vm".to_owned(),
            curves: Vec::new(),
            lanes: Vec::new(),
        },
    )
    .unwrap();
    let history = History::load(&path).unwrap();
    let p = plan(&cmd(&jobs), &Laptop::default(), Some(&vm()), &history);
    let lanes: Vec<Lane> = p.jobs.iter().map(|j| j.lane).collect();
    assert_eq!(lanes, [Lane::Max, Lane::Short]);
    let sources: Vec<Option<Source>> = p.jobs.iter().map(|j| j.source).collect();
    assert_eq!(sources, [Some(Source::Suffix), Some(Source::Suffix)]);
    assert_eq!(p.warnings.len(), 1);
}

#[test]
fn lanes_order_unknown_between_short_and_long() {
    assert!(Lane::Short < Lane::Unknown && Lane::Unknown < Lane::Long && Lane::Long < Lane::Max);
    assert_eq!(Lane::from(None), Lane::Unknown);
    assert_eq!(Lane::from(Some(Expected::Long)), Lane::Long);
}

#[test]
fn history_max_jobs_are_set_aside_and_their_older_versions_take_no_part() {
    let jobs = [job("r", &["R.cfg"]), job("q", &["Q_max.cfg", "P.cfg"])];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    History::append(
        &path,
        &crate::history::Record {
            id: "old-r".to_owned(),
            end: "v1".to_owned(),
            names: vec![jobs[0].names[0].keys.clone()],
            expected: Some(Expected::Max),
            lower_bound: false,
            duration_secs: 7200,
            outcome: "ok".to_owned(),
            place: "vm".to_owned(),
            curves: Vec::new(),
            lanes: Vec::new(),
        },
    )
    .unwrap();
    let history = History::load(&path).unwrap();
    let mut reply = vm();
    reply.older.push(VmOlder {
        id: "old-r".to_owned(),
        key: key("/w1", "R.cfg"),
        running: true,
        place: "vm".to_owned(),
        elapsed_secs: 60,
        counters: None,
    });
    let mut skipping = cmd(&jobs);
    skipping.skip_max = true;
    let p = plan(&skipping, &Laptop::default(), Some(&reply), &history);
    assert_eq!(p.skipped, ["r"]);
    assert_eq!(p.refusals, []);
    // Q_max.cfg and P.cfg share an id: not every name is max, so it stays.
    assert_eq!(
        p.jobs.iter().map(|j| j.id.as_str()).collect::<Vec<_>>(),
        ["q"]
    );
    let named = plan(&cmd(&jobs), &Laptop::default(), Some(&reply), &history);
    assert_eq!(named.skipped, Vec::<String>::new());
    assert_eq!(named.refusals.len(), 1);
}

#[test]
fn a_supersede_never_cancels_a_job_this_command_attaches_to() {
    // A.cfg was T (job o, queued on the laptop); B.cfg is now a copy of T, A.cfg edited.
    let jobs = [job("x", &["A.cfg"]), job("o", &["B.cfg"])];
    let mut laptop = Laptop::default();
    laptop_job(&mut laptop, "o", "A.cfg", false);
    let p = plan(&cmd(&jobs), &laptop, Some(&vm()), &History::default());
    assert_eq!(
        kinds(&p).get(1).map(|(k, r)| ((*k).clone(), *r)),
        Some((Kind::AttachLaptop, Route::LaptopAttach))
    );
    assert_eq!(
        p.supersede,
        [Supersede {
            old: "o".to_owned(),
            key: key("/w1", "A.cfg"),
            on_laptop: true,
            by: "x".to_owned(),
            attached: true
        }]
    );
}

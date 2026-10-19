//! End-to-end tests of placement against a runner in the same process.

use std::path::{Path, PathBuf};

use super::*;
use crate::batch::Named;
use crate::config::{Expect, JobOptions};
use crate::entry::Label;
use crate::history::Keys;
use crate::job::{Contents, File, Limits};
use crate::outcome::Outcome;
use crate::plan::{Command, Refusal};
use crate::proto::{PreflightReply, PreflightRequest};
use crate::remote::CallError;
use crate::runner::verbs::tests::{runner, Fake};
use crate::runner::verbs::{container_name, Runner};

/// The runner, reached in-process; it can be made unreachable, lose a commit's
/// reply, hold a commit back (as a slow connection would, its client giving
/// up), or change its jars between the preflight and the commit.
pub(crate) struct Local {
    pub(crate) runner: Runner<Fake>,
    pub(crate) fake: Fake,
    pub(crate) down: bool,
    pub(crate) lose_reply: bool,
    pub(crate) hold: bool,
    pub(crate) held: Option<Vec<u8>>,
    pub(crate) jars_after_preflight: Option<String>,
    pub(crate) finish_after_preflight: Option<String>,
}

impl Remote for Local {
    fn preflight(&mut self, req: &PreflightRequest) -> Result<PreflightReply, CallError> {
        if self.down {
            return Err(CallError::Unreachable("no route".to_owned()));
        }
        let reply = self
            .runner
            .preflight(req)
            .map_err(|e| CallError::Failed(e.to_string()));
        if let Some(jars) = self.jars_after_preflight.take() {
            self.runner.versions = crate::engine::test_versions(&jars);
        }
        if let Some(id) = self.finish_after_preflight.take() {
            finish(&self.runner, &self.fake, &id, Outcome::Ok);
        }
        reply
    }

    fn status(&mut self, req: &PreflightRequest) -> Result<PreflightReply, CallError> {
        if self.down {
            return Err(CallError::Unreachable("no route".to_owned()));
        }
        self.runner
            .status(req)
            .map_err(|e| CallError::Failed(e.to_string()))
    }

    fn commit(&mut self, wire: &[u8]) -> Result<CommitReply, CallError> {
        if self.hold {
            self.held = Some(wire.to_vec());
            return Err(CallError::Failed("the deadline passed".to_owned()));
        }
        let reply = self
            .runner
            .commit(wire)
            .map_err(|e| CallError::Failed(e.to_string()))?;
        if self.lose_reply {
            return Err(CallError::Failed("the connection dropped".to_owned()));
        }
        Ok(reply)
    }

    fn ls(&mut self) -> Result<std::collections::BTreeMap<String, crate::proto::VmJob>, CallError> {
        if self.down {
            return Err(CallError::Unreachable("no route".to_owned()));
        }
        self.runner
            .ls()
            .map_err(|e| CallError::Failed(e.to_string()))
    }

    fn cancel(
        &mut self,
        req: &crate::proto::CancelRequest,
    ) -> Result<crate::proto::VmJob, CallError> {
        if self.down {
            return Err(CallError::Unreachable("no route".to_owned()));
        }
        self.runner
            .cancel(req)
            .map_err(|e| CallError::Failed(e.to_string()))
    }

    fn resume(
        &mut self,
        req: &crate::proto::ResumeRequest,
    ) -> Result<crate::proto::ResumeReply, CallError> {
        if self.down {
            return Err(CallError::Unreachable("no route".to_owned()));
        }
        self.runner
            .resume_run(req)
            .map_err(|e| CallError::Failed(e.to_string()))
    }

    fn fetch(&mut self, req: &crate::proto::FetchRequest) -> Result<Vec<u8>, CallError> {
        if self.down {
            return Err(CallError::Unreachable("no route".to_owned()));
        }
        let mut out = Vec::new();
        self.runner
            .fetch(req, &mut out)
            .map_err(|e| CallError::Failed(e.to_string()))?;
        Ok(out)
    }
}

/// Supervisor processes that only hold their job's lock: killing one lets
/// the lock go.
#[derive(Default)]
pub(crate) struct Procs {
    locks: std::collections::BTreeMap<u32, std::fs::File>,
    pub(crate) launched: Vec<String>,
    pub(crate) killed: Vec<u32>,
}

impl crate::laptop_runs::Processes for Procs {
    fn spawn(&mut self, _: &Path, id: &str, _: &str, lock: std::fs::File) -> std::io::Result<u32> {
        let pid = 1000 + u32::try_from(self.launched.len()).map_err(std::io::Error::other)?;
        self.locks.insert(pid, lock);
        self.launched.push(id.to_owned());
        Ok(pid)
    }

    fn kill(&mut self, pid: u32) -> std::io::Result<()> {
        self.locks.remove(&pid);
        self.killed.push(pid);
        Ok(())
    }
}

/// The laptop's runs, their supervisors [`Procs`].
pub(crate) type Sups = crate::laptop_runs::LaptopRuns<Procs>;

pub(crate) struct World {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) store: Store,
    pub(crate) vm: Local,
    pub(crate) sups: Sups,
}

pub(crate) fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("laptop")).unwrap();
    let (runner, fake) = runner(&dir.path().join("vm"));
    World {
        dir,
        store,
        vm: Local {
            runner,
            fake,
            down: false,
            lose_reply: false,
            hold: false,
            held: None,
            jars_after_preflight: None,
            finish_after_preflight: None,
        },
        sups: Sups {
            processes: Procs::default(),
        },
    }
}

/// A job checking `cfg_name` whose configuration reads `text`, so two texts
/// make two versions of one configuration.
pub(crate) fn job(cfg_name: &str, text: &str, expect: Option<Expect>) -> Job {
    let contents = Contents::new(
        vec![
            File {
                path: "model/M.tla".to_owned(),
                bytes: b"---- MODULE M ----\n====\n".to_vec(),
            },
            File {
                path: crate::job::CFG.to_owned(),
                bytes: text.as_bytes().to_vec(),
            },
        ],
        Limits::default(),
    )
    .unwrap();
    let manifest = crate::id::manifest("M", &contents, &[], "tlc J");
    Job {
        id: crate::id::id(&manifest),
        engine: crate::engine::Engine::Tlc,
        names: vec![Named {
            name: cfg_name.to_owned(),
            keys: Keys::new(
                crate::engine::Engine::Tlc,
                &manifest,
                &contents,
                &[],
                "M",
                cfg_name,
            ),
            options: JobOptions {
                expect,
                ..JobOptions::default()
            },
        }],
        manifest,
        contents,
    }
}

pub(crate) fn label() -> Label {
    Label {
        project: "p".to_owned(),
        worktree: PathBuf::from("/w1"),
        branch: Some("main".to_owned()),
        agent: None,
    }
}

pub(crate) fn submit_in(
    w: &mut World,
    jobs: &[Job],
    slots: usize,
    replace: bool,
    token: &str,
) -> Report {
    let s = Submit {
        cmd: Command {
            jobs,
            worktree: PathBuf::from("/w1"),
            model_dir: PathBuf::from("tla/m"),
            label: label(),
            replace,
            local: false,
            skip_max: false,
        },
        versions: crate::engine::test_versions("J"),
        vm_on_timeout: false,
        slots,
        token: token.to_owned(),
    };
    submit(&w.store, &mut w.vm, &mut w.sups, &s).unwrap()
}

fn record(w: &World, id: &str) -> JobRecord {
    w.store.record(id).unwrap().unwrap()
}

fn follows(w: &World) -> Vec<(String, Followed)> {
    w.store
        .follows()
        .unwrap()
        .into_iter()
        .map(|f| (f.id, f.what))
        .collect()
}

fn run(name: &str) -> Followed {
    Followed::Run(RunRef::Run(name.to_owned()))
}

/// End the VM's running attempt of `id` with `outcome`, as its container would.
pub(crate) fn finish(runner: &Runner<Fake>, fake: &Fake, id: &str, outcome: Outcome) {
    let dir: &Path = &runner.store.attempt_dir(id, "v1", 1);
    std::fs::write(dir.join("started"), "").unwrap();
    let result = crate::runner::store::AttemptResult {
        finished: Some(crate::runner::state::State::outcome_only(outcome)),
        ..Default::default()
    };
    crate::files::write_json(&dir.join("result.json"), &result).unwrap();
    fake.gone(&container_name(id, "v1", 1));
}

#[test]
fn a_short_job_with_a_slot_starts_on_an_idle_vm_and_is_attached_to_after() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", Some(Expect::Fast));
    let report = submit_in(&mut w, std::slice::from_ref(&x), 1, false, "t1");
    assert_eq!(report.exit, 0);
    assert_eq!(
        report.placed[&x.id],
        Placed::Vm(JobAnswer::Started {
            run: "v1".to_owned()
        })
    );
    let r = record(&w, &x.id);
    assert_eq!(
        (r.state, r.run.as_deref(), r.token.as_deref()),
        (State::Vm, Some("v1"), None)
    );
    assert_eq!(follows(&w), [(x.id.clone(), run("v1"))]);
    let again = submit_in(&mut w, std::slice::from_ref(&x), 1, false, "t2");
    assert_eq!(
        again.placed[&x.id],
        Placed::Vm(JobAnswer::Attached {
            run: "v1".to_owned()
        })
    );
    assert_eq!(
        follows(&w).len(),
        1,
        "the same agent, key and run add no follow"
    );
}

#[test]
fn a_refused_now_or_refuse_job_runs_on_the_laptop_slot_it_held() {
    let mut w = world();
    w.vm.fake.0.borrow_mut().fail = true;
    let x = job("A.cfg", "INIT Init\n", Some(Expect::Fast));
    let report = submit_in(&mut w, std::slice::from_ref(&x), 1, false, "t1");
    assert_eq!(report.placed[&x.id], Placed::Laptop("l1".to_owned()));
    let r = record(&w, &x.id);
    assert_eq!((r.state, r.run.as_deref()), (State::Laptop, Some("l1")));
    assert_eq!(w.sups.processes.launched, std::slice::from_ref(&x.id));
    assert_eq!(follows(&w), [(x.id.clone(), run("l1"))]);
}

#[test]
fn an_ended_vm_job_is_a_cache_hit_next_time() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    finish(&w.vm.runner, &w.vm.fake, &x.id, Outcome::Ok);
    let report = submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t2");
    let Placed::Cached(result) = &report.placed[&x.id] else {
        panic!("not a cache hit: {:?}", report.placed);
    };
    assert_eq!(result.outcome, Outcome::Ok);
    assert_eq!(record(&w, &x.id).state, State::Ended);
    assert!(w.store.cached(&x.id).unwrap().is_some());
    let history = History::load(&w.store.history()).unwrap();
    assert_eq!(history.records().len(), 1);
    assert!(matches!(follows(&w).last(), Some((_, Followed::Result(_)))));
    // A laptop that never saw the run (here: forgot it) takes the VM's cached
    // result into its own cache with the hit.
    let lock = w.store.placement_lock().unwrap();
    w.store.remove_record(&lock, &x.id).unwrap();
    drop(lock);
    let cached = w
        .dir
        .path()
        .join("laptop/results")
        .join(format!("{}.json", x.id));
    std::fs::remove_file(&cached).unwrap();
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t3");
    assert_eq!(
        w.store.cached(&x.id).unwrap().map(|r| r.outcome),
        Some(Outcome::Ok)
    );
}

#[test]
fn an_unreachable_vm_runs_unknown_jobs_on_the_laptop_and_refuses_long_ones() {
    let mut w = world();
    w.vm.down = true;
    let x = job("A.cfg", "INIT Init\n", None);
    let report = submit_in(&mut w, std::slice::from_ref(&x), 1, false, "t1");
    assert_eq!(report.placed[&x.id], Placed::Laptop("l1".to_owned()));
    let long = job("B.cfg", "INIT Init\n\\* long\n", Some(Expect::Slow));
    let refused = submit_in(&mut w, std::slice::from_ref(&long), 1, false, "t2");
    assert_eq!(refused.exit, 2);
    assert_eq!(
        refused.plan.refusals,
        [Refusal::LongOffline {
            id: long.id.clone()
        }]
    );
    assert!(w.store.record(&long.id).unwrap().is_none());
}

#[test]
fn an_invalid_answer_undoes_the_write_ahead() {
    let mut w = world();
    w.vm.jars_after_preflight = Some("K".to_owned());
    let x = job("A.cfg", "INIT Init\n", None);
    let report = submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    assert_eq!(report.exit, 2);
    assert_eq!(
        report.invalid.as_ref().map(|(id, _)| id.as_str()),
        Some(x.id.as_str())
    );
    assert!(w.store.record(&x.id).unwrap().is_none());
    assert_eq!(follows(&w), []);
}

#[test]
fn a_lost_reply_keeps_the_write_ahead_until_a_lookup_resolves_it() {
    let mut w = world();
    w.vm.lose_reply = true;
    let x = job("A.cfg", "INIT Init\n", None);
    let report = submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    assert_eq!(report.exit, 3);
    let r = record(&w, &x.id);
    assert_eq!((r.state, r.token.as_deref()), (State::Sent, Some("t1")));
    assert_eq!(
        follows(&w),
        [(x.id.clone(), Followed::Run(RunRef::Token("t1".to_owned())))]
    );
    w.vm.lose_reply = false;
    let other = job("B.cfg", "INIT Other\n", None);
    submit_in(&mut w, std::slice::from_ref(&other), 0, false, "t2");
    let r = record(&w, &x.id);
    assert_eq!((r.state, r.run.as_deref()), (State::Vm, Some("v1")));
    assert_eq!(follows(&w)[0], (x.id.clone(), run("v1")));
}

#[test]
fn a_new_version_supersedes_the_old_one_on_the_vm_with_replace() {
    let mut w = world();
    let old = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&old), 0, false, "t1");
    let new = job("A.cfg", "INIT Init2\n", None);
    let refused = submit_in(&mut w, std::slice::from_ref(&new), 0, false, "t2");
    assert_eq!(
        refused.plan.refusals,
        [Refusal::OlderRunning {
            id: new.id.clone(),
            old: old.id.clone()
        }]
    );
    let report = submit_in(&mut w, std::slice::from_ref(&new), 0, true, "t3");
    assert_eq!(report.exit, 0);
    assert_eq!(
        report.placed[&new.id],
        Placed::Vm(JobAnswer::Queued {
            run: "v1".to_owned()
        })
    );
    assert_eq!(
        w.vm.fake.0.borrow().stopped,
        [container_name(&old.id, "v1", 1)]
    );
}

#[test]
fn a_new_version_supersedes_the_old_one_on_the_laptop() {
    let mut w = world();
    w.vm.down = true;
    let old = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&old), 1, false, "t1");
    let new = job("A.cfg", "INIT Init2\n", None);
    let report = submit_in(&mut w, std::slice::from_ref(&new), 1, true, "t2");
    assert_eq!(report.exit, 0);
    assert_eq!(w.sups.processes.killed, [1000]);
    let cancelled = record(&w, &old.id).result.unwrap().outcome;
    assert_eq!(
        cancelled,
        Outcome::Cancelled {
            by: "p/main".to_owned(),
            replaced_by: Some(new.id.clone())
        }
    );
    assert_eq!(report.placed[&new.id], Placed::Laptop("l1".to_owned()));
}

#[test]
fn a_pending_entry_the_vm_drops_is_not_mirrored() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    w.vm.down = true;
    let mut b = x.clone();
    b.names[0].name = "B.cfg".to_owned();
    b.names[0].keys.cfg_name = "B.cfg".to_owned();
    submit_in(&mut w, std::slice::from_ref(&b), 0, false, "t2");
    w.vm.down = false;
    // It names a run the VM no longer holds live.
    let lock = w.store.placement_lock().unwrap();
    let mut r = record(&w, &x.id);
    r.pending[0].run = RunRef::Run("v9".to_owned());
    w.store.write_record(&lock, &mut r).unwrap();
    drop(lock);
    let other = job("C.cfg", "INIT Other\n", None);
    let flushed = submit_in(&mut w, std::slice::from_ref(&other), 0, false, "t3");
    assert_eq!(flushed.dropped.len(), 1);
    assert_eq!(record(&w, &x.id).pending, []);
    assert_eq!(record(&w, &x.id).mirror.len(), 1);
}

#[test]
fn an_offline_attach_is_pending_until_the_next_commit_flushes_it() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    w.vm.down = true;
    let mut b = x.clone();
    b.names[0].name = "B.cfg".to_owned();
    b.names[0].keys.cfg_name = "B.cfg".to_owned();
    let report = submit_in(&mut w, std::slice::from_ref(&b), 0, false, "t2");
    assert_eq!(
        report.placed[&x.id],
        Placed::Pending(RunRef::Run("v1".to_owned()))
    );
    assert_eq!(record(&w, &x.id).pending.len(), 1);
    w.vm.down = false;
    let other = job("C.cfg", "INIT Other\n", None);
    let flushed = submit_in(&mut w, std::slice::from_ref(&other), 0, false, "t3");
    assert_eq!(flushed.dropped, []);
    assert_eq!(record(&w, &x.id).pending, []);
    assert_eq!(record(&w, &x.id).mirror.len(), 2);
    let lock = w.vm.runner.store.lock().unwrap();
    let state = w.vm.runner.store.load(&lock).unwrap();
    assert_eq!(state.latest(&x.id).unwrap().entries.len(), 2);
}

#[test]
fn a_job_the_vm_caches_between_preflight_and_commit_is_answered_cached() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    w.vm.finish_after_preflight = Some(x.id.clone());
    let mut b = x.clone();
    b.names[0].name = "B.cfg".to_owned();
    b.names[0].keys.cfg_name = "B.cfg".to_owned();
    let report = submit_in(&mut w, std::slice::from_ref(&b), 0, false, "t2");
    assert!(matches!(
        &report.placed[&x.id],
        Placed::Vm(JobAnswer::Cached { .. })
    ));
    assert_eq!(record(&w, &x.id).state, State::Ended);
    assert!(matches!(follows(&w).last(), Some((_, Followed::Result(_)))));
}

#[test]
fn an_invalid_answer_restores_an_attached_record() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    let before = record(&w, &x.id);
    w.vm.jars_after_preflight = Some("K".to_owned());
    let mut b = x.clone();
    b.names[0].name = "B.cfg".to_owned();
    b.names[0].keys.cfg_name = "B.cfg".to_owned();
    let report = submit_in(&mut w, std::slice::from_ref(&b), 0, false, "t2");
    assert_eq!(report.exit, 2);
    let after = record(&w, &x.id);
    assert_eq!(
        (after.state, &after.run, &after.mirror),
        (before.state, &before.run, &before.mirror)
    );
    assert_eq!(follows(&w).len(), 1);
}

#[test]
fn an_unknown_outcome_starts_nothing_on_the_laptop_but_keeps_laptop_attaches() {
    let mut w = world();
    w.vm.down = true;
    let laptop = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&laptop), 2, false, "t1");
    w.vm.down = false;
    w.vm.lose_reply = true;
    let mut attach = laptop.clone();
    attach.names[0].name = "B.cfg".to_owned();
    attach.names[0].keys.cfg_name = "B.cfg".to_owned();
    let sent = job("C.cfg", "INIT Other\n", Some(Expect::Slow));
    let fresh = job("D.cfg", "INIT Fresh\n", None);
    let jobs = [attach, sent, fresh.clone()];
    let report = submit_in(&mut w, &jobs, 2, false, "t2");
    assert_eq!(report.exit, 3);
    assert_eq!(
        report.placed[&laptop.id],
        Placed::LaptopAttach("l1".to_owned())
    );
    assert_eq!(report.placed[&fresh.id], Placed::NotStarted);
    assert_eq!(record(&w, &laptop.id).entries.len(), 2);
    assert!(w.store.record(&fresh.id).unwrap().is_none());
    assert_eq!(w.sups.processes.launched.len(), 1);
}

#[test]
fn lanes_go_into_job_json_as_expectations() {
    assert_eq!(expected(Lane::Short), Some(Expected::Short));
    assert_eq!(expected(Lane::Unknown), None);
    assert_eq!(expected(Lane::Long), Some(Expected::Long));
    assert_eq!(expected(Lane::Max), Some(Expected::Max));
}

#[test]
fn the_plan_reads_the_history_the_refresh_just_wrote() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    // The run times out at its 2-hour cap, and nobody polls.
    let dir = w.vm.runner.store.attempt_dir(&x.id, "v1", 1);
    std::fs::write(dir.join("started"), "").unwrap();
    std::fs::write(dir.join("timed-out"), "").unwrap();
    let so_far = crate::runner::store::AttemptResult {
        duration_secs: 7200,
        ..Default::default()
    };
    crate::files::write_json(&dir.join("result.json"), &so_far).unwrap();
    w.vm.fake.gone(&container_name(&x.id, "v1", 1));
    let report = submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t2");
    assert_eq!(report.plan.jobs[0].lane, Lane::Max);
}

#[test]
fn a_new_run_written_ahead_keeps_nothing_of_an_earlier_run() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    let lock = w.store.placement_lock().unwrap();
    let mut old = JobRecord::new(&x.id, State::Ended);
    old.mirror = vec![crate::runner::state::tests::entry("A.cfg")];
    old.result = Some(crate::runner::state::State::outcome_only(Outcome::Timeout));
    w.store.write_record(&lock, &mut old).unwrap();
    drop(lock);
    w.vm.lose_reply = true;
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    let r = record(&w, &x.id);
    assert_eq!(r.state, State::Sent);
    assert_eq!(
        r.mirror.iter().map(|e| e.label.clone()).collect::<Vec<_>>(),
        [label()]
    );
    assert_eq!(r.result, None);
}

#[test]
fn an_attach_with_other_flags_keeps_the_first_submitter_s_and_warns() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    let mut again = x.clone();
    again.names[0].name = "B.cfg".to_owned();
    again.names[0].keys.cfg_name = "B.cfg".to_owned();
    again.names[0].options.timeout = Some(std::time::Duration::from_secs(3600));
    let report = submit_in(&mut w, std::slice::from_ref(&again), 0, false, "t2");
    assert_eq!(report.plan.warnings.len(), 1, "{:?}", report.plan.warnings);
    assert!(
        report.plan.warnings[0].contains("timeout none")
            && report.plan.warnings[0].contains("timeout 3600s")
    );
    let same = submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t3");
    assert_eq!(same.plan.warnings, Vec::<String>::new());
}

#[test]
fn a_vm_supersede_drops_the_key_from_the_old_job_s_mirror() {
    let mut w = world();
    let old = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&old), 0, false, "t1");
    assert_eq!(record(&w, &old.id).mirror.len(), 1);
    let new = job("A.cfg", "INIT Init2\n", None);
    submit_in(&mut w, std::slice::from_ref(&new), 0, true, "t2");
    assert_eq!(record(&w, &old.id).mirror, []);
    // Offline now, the same configuration placed again is no older version's.
    w.vm.down = true;
    let again = submit_in(&mut w, std::slice::from_ref(&new), 0, false, "t3");
    assert_eq!(again.plan.refusals, []);
}

#[test]
fn a_vm_supersede_of_a_job_the_laptop_has_no_record_of_writes_none() {
    let mut w = world();
    let old = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&old), 0, false, "t1");
    let lock = w.store.placement_lock().unwrap();
    w.store.remove_record(&lock, &old.id).unwrap();
    drop(lock);
    let new = job("A.cfg", "INIT Init2\n", None);
    let report = submit_in(&mut w, std::slice::from_ref(&new), 0, true, "t2");
    assert_eq!(report.exit, 0);
    assert!(w.store.record(&old.id).unwrap().is_none());
}

#[test]
fn a_placement_counts_as_a_duplicate_only_of_the_latest_follow_under_its_key() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    // A later placement under the key that never reached the VM.
    let lock = w.store.placement_lock().unwrap();
    let never = Follow {
        number: 2,
        key: crate::runner::state::tests::entry("A.cfg").key,
        label: label(),
        id: x.id.clone(),
        what: Followed::NeverSent,
    };
    w.store.write_follow(&lock, &never).unwrap();
    drop(lock);
    // The resubmit attaches to v1 again: its follow is kept, as the latest.
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t2");
    let latest = w
        .store
        .follows()
        .unwrap()
        .into_iter()
        .max_by_key(|f| f.number)
        .unwrap();
    assert_eq!((latest.number, latest.what), (3, run("v1")));
    // Placed once more, the latest already follows v1: no follow is added.
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t3");
    assert_eq!(w.store.follows().unwrap().len(), 3);
}

#[test]
fn a_run_replaced_while_stopping_still_records_its_end() {
    let mut w = world();
    let x = job("A.cfg", "INIT Init\n", None);
    submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t1");
    let cancel = crate::proto::CancelRequest {
        id: x.id.clone(),
        run: Some("v1".to_owned()),
        by: "agent".to_owned(),
    };
    w.vm.runner.cancel(&cancel).unwrap();
    // Placed again while v1 still stops: a new run, v2.
    let again = submit_in(&mut w, std::slice::from_ref(&x), 0, false, "t2");
    assert_eq!(
        again.placed[&x.id],
        Placed::Vm(JobAnswer::Queued {
            run: "v2".to_owned()
        })
    );
    assert_eq!(record(&w, &x.id).ending, ["v1"]);
    // v1's container goes; the next preflight records its end.
    let dir = w.vm.runner.store.attempt_dir(&x.id, "v1", 1);
    std::fs::write(dir.join("started"), "").unwrap();
    w.vm.fake.gone(&container_name(&x.id, "v1", 1));
    let other = job("B.cfg", "INIT Other\n", None);
    submit_in(&mut w, std::slice::from_ref(&other), 0, false, "t3");
    let history = History::load(&w.store.history()).unwrap();
    let ends: Vec<(&str, &str)> = history
        .records()
        .iter()
        .map(|r| (r.end.as_str(), r.outcome.as_str()))
        .collect();
    assert_eq!(ends, [("v1", "cancelled")]);
    assert_eq!(record(&w, &x.id).ending, Vec::<String>::new());
}

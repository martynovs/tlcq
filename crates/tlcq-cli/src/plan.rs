//! Placement, decided under the placement lock after the preflight's refresh
//! (design D6: the classification of each job, the refusals, the supersedes and
//! the routes). Pure: it reads a snapshot and returns a plan, which the command
//! then carries out. The protocol it is part of is model-checked
//! (`tla/placement/Placement.tla`).

use std::collections::{BTreeMap, BTreeSet};

use crate::batch::Job;
use crate::engine::Engine;
use crate::entry::{Entry, Key, Label};
use crate::history::{self, Expected, History, Source};
use crate::outcome::JobResult;
use crate::proto::{PreflightReply, VmState};
use crate::store::{JobRecord, RunRef, State};

/// A job's expected duration, ordered short < unknown < long < max (D5), the
/// order that merges the names of one job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lane {
    /// Under 5 minutes.
    Short,
    /// No flag, suffix or history says.
    Unknown,
    /// Over 5 minutes.
    Long,
    /// Over an hour.
    Max,
}

impl From<Option<Expected>> for Lane {
    fn from(e: Option<Expected>) -> Self {
        match e {
            None => Lane::Unknown,
            Some(Expected::Short) => Lane::Short,
            Some(Expected::Long) => Lane::Long,
            Some(Expected::Max) => Lane::Max,
        }
    }
}

/// What a job is, against what the laptop and the VM already hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A result is cached for its id.
    CacheHit(JobResult),
    /// Queued or running on the VM: the commit attaches to it.
    AttachVm,
    /// A laptop job with a live supervisor: an entry on its record.
    AttachLaptop,
    /// The VM is unreachable and the laptop holds the job as on (or maybe on) the
    /// VM: a pending entry naming that run.
    AttachOffline(RunRef),
    /// Nothing holds it: it is placed anew.
    New,
}

/// Where a job goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Nowhere: a cache hit.
    None,
    /// The laptop, for its slot.
    Laptop,
    /// The VM's queue.
    VmQueue,
    /// The VM if it starts it now, else refused, to run on the laptop slot it holds.
    VmNowOrRefuse,
    /// The VM, now or in its queue.
    VmNowOrQueue,
    /// An entry on the job the laptop runs.
    LaptopAttach,
    /// An attach in the commit.
    VmAttach,
    /// A pending entry, for the next commit.
    Pending,
}

/// A job and what to do with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    /// The job's id.
    pub id: String,
    /// Its expected duration, merged over its names.
    pub lane: Lane,
    /// Where it came from, for `job.json` and the reports (`None`: unknown).
    pub source: Option<Source>,
    /// What it is.
    pub kind: Kind,
    /// Where it goes.
    pub route: Route,
    /// It asks for memory: only the VM runs it (job-memory M5).
    pub vm_only: bool,
}

/// Why a command is refused (all or nothing: one refusal refuses it all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The VM's jars differ from the laptop's, and a job runs on TLC.
    JarMismatch,
    /// The VM's tlx rev differs from the laptop's, and a job runs on tlx.
    TlxMismatch,
    /// The VM runs a tlcq older than engines: no id of this laptop matches its.
    RunnerOutdated,
    /// A long or max job while the VM is unreachable, without `--local`.
    LongOffline {
        /// The job.
        id: String,
    },
    /// A job asking for memory that would run on the laptop: `--local`, or
    /// the VM unreachable.
    MemoryOnLaptop {
        /// The job.
        id: String,
    },
    /// An older version of the configuration runs, and `--replace` was not given.
    OlderRunning {
        /// The new job.
        id: String,
        /// The older version.
        old: String,
    },
    /// An older version that may run on the VM, while the VM is unreachable:
    /// refused with or without `--replace`.
    ReplaceUnreachable {
        /// The new job.
        id: String,
        /// The older version.
        old: String,
    },
}

/// An older version to supersede: this worktree's entry under `key` removed
/// from `old`, which is cancelled if no entry is left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supersede {
    /// The older version.
    pub old: String,
    /// The key whose entry goes.
    pub key: Key,
    /// Whether it is a laptop job (superseded before the commit) or a VM one (in it).
    pub on_laptop: bool,
    /// The new id, for "replaced by".
    pub by: String,
    /// Whether this command also attaches to it: then it is never cancelled, and
    /// on the laptop the attach's entry is written in the same record write as the
    /// supersede, before the commit, so no cut-off command or `invalid` answer can
    /// leave it with no entry (on the VM, jobs come before supersedes in the commit).
    pub attached: bool,
}

/// The plan for a command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Each job, in submit order.
    pub jobs: Vec<Planned>,
    /// Why the command is refused, if it is.
    pub refusals: Vec<Refusal>,
    /// The older versions to supersede.
    pub supersede: Vec<Supersede>,
    /// Warnings to report (a suffix the history contradicts).
    pub warnings: Vec<String>,
    /// Jobs set aside because every name is max, by suffix or history: listed,
    /// and taking no further part.
    pub skipped: Vec<String>,
}

/// What the laptop holds, after the refresh.
#[derive(Debug, Clone, Default)]
pub struct Laptop {
    /// Its job records, by id.
    pub records: BTreeMap<String, JobRecord>,
    /// Ids whose job lock a supervisor holds.
    pub locked: BTreeSet<String>,
    /// Laptop jobs that run (or move), rather than wait for the slot.
    pub running: BTreeSet<String>,
    /// The laptop's cache, for the batch's ids.
    pub cache: BTreeMap<String, JobResult>,
    /// Laptop slots free: the slot count minus laptop jobs running or waiting.
    pub free_slots: usize,
}

/// The command's own inputs.
#[derive(Debug, Clone)]
pub struct Command<'a> {
    /// The jobs, from the input checks.
    pub jobs: &'a [Job],
    /// The worktree's path.
    pub worktree: std::path::PathBuf,
    /// The model directory, relative to the worktree.
    pub model_dir: std::path::PathBuf,
    /// The agent.
    pub label: Label,
    /// `--replace`.
    pub replace: bool,
    /// `--local`.
    pub local: bool,
    /// No configuration named and no `--all`: max jobs are set aside.
    pub skip_max: bool,
}

impl Command<'_> {
    /// The entries a job places: one per configuration name.
    pub fn entries(&self, job: &Job) -> Vec<Entry> {
        job.names
            .iter()
            .map(|n| Entry {
                key: Key {
                    worktree: self.worktree.clone(),
                    model_dir: self.model_dir.clone(),
                    spec: n.keys.spec.clone(),
                    cfg_name: n.name.clone(),
                },
                label: self.label.clone(),
            })
            .collect()
    }
}

/// Plan `cmd` against the laptop and the VM's reply (`None`: unreachable).
pub fn plan(
    cmd: &Command<'_>,
    laptop: &Laptop,
    vm: Option<&PreflightReply>,
    history: &History,
) -> Plan {
    let mut plan = Plan::default();
    // Only the engines the command's jobs use must match (tlx-engine E2).
    let uses = |engine| cmd.jobs.iter().any(|j| j.engine == engine);
    if let Some(r) = vm {
        match r.tlx_match {
            None => plan.refusals.push(Refusal::RunnerOutdated),
            Some(tlx_match) => {
                if !r.jars_match && uses(Engine::Tlc) {
                    plan.refusals.push(Refusal::JarMismatch);
                }
                if !tlx_match && uses(Engine::Tlx) {
                    plan.refusals.push(Refusal::TlxMismatch);
                }
            }
        }
    }
    for job in cmd.jobs {
        if cmd.skip_max && every_name_max(job, history) {
            plan.skipped.push(job.id.clone());
            continue;
        }
        let (lane, source) = lane(job, history, &mut plan.warnings);
        let kind = classify(job, laptop, vm);
        let vm_only = job.memory_mb().is_some();
        if kind == Kind::New && vm_only && (cmd.local || vm.is_none()) {
            plan.refusals
                .push(Refusal::MemoryOnLaptop { id: job.id.clone() });
        } else if kind == Kind::New && vm.is_none() && !cmd.local && lane >= Lane::Long {
            plan.refusals
                .push(Refusal::LongOffline { id: job.id.clone() });
        }
        if !matches!(kind, Kind::CacheHit(_)) {
            for entry in cmd.entries(job) {
                for (old, running) in older(&job.id, &entry.key, laptop, vm) {
                    let on_vm = laptop
                        .records
                        .get(&old)
                        .is_none_or(|r| r.state != State::Laptop);
                    // Offline, an older version on the VM can be neither
                    // superseded nor waited on: refused with or without
                    // `--replace`, for the VM being unreachable.
                    if running && vm.is_none() && on_vm {
                        plan.refusals.push(Refusal::ReplaceUnreachable {
                            id: job.id.clone(),
                            old,
                        });
                    } else if running && !cmd.replace {
                        plan.refusals.push(Refusal::OlderRunning {
                            id: job.id.clone(),
                            old,
                        });
                    } else {
                        plan.supersede.push(Supersede {
                            old,
                            key: entry.key.clone(),
                            on_laptop: !on_vm,
                            by: job.id.clone(),
                            attached: false,
                        });
                    }
                }
            }
        }
        plan.jobs.push(Planned {
            id: job.id.clone(),
            lane,
            source,
            kind,
            route: Route::None,
            vm_only,
        });
    }
    // A job this command attaches to only loses the superseded entry: the
    // attach's own entry keeps it alive, as jobs come before supersedes in a commit.
    let attached: BTreeSet<&str> = plan
        .jobs
        .iter()
        .filter(|j| {
            matches!(
                j.kind,
                Kind::AttachLaptop | Kind::AttachVm | Kind::AttachOffline(_)
            )
        })
        .map(|j| j.id.as_str())
        .collect();
    for s in &mut plan.supersede {
        s.attached = attached.contains(s.old.as_str());
    }
    route(&mut plan.jobs, laptop.free_slots, vm.is_some(), cmd.local);
    plan
}

/// Whether every name of `job` is expected max, by suffix or history.
fn every_name_max(job: &Job, history: &History) -> bool {
    job.names.iter().all(|n| {
        matches!(
            history::expectation(n.options.expect, &n.keys, history).0,
            Some((Expected::Max, _))
        )
    })
}

/// A job's lane, the longest of its names' (short < unknown < long < max), with
/// the source of the name that set it.
fn lane(job: &Job, history: &History, warnings: &mut Vec<String>) -> (Lane, Option<Source>) {
    job.names
        .iter()
        .map(|n| {
            let (chosen, warning) = history::expectation(n.options.expect, &n.keys, history);
            warnings.extend(warning);
            (Lane::from(chosen.map(|(e, _)| e)), chosen.map(|(_, s)| s))
        })
        .max_by_key(|(lane, _)| *lane)
        .unwrap_or((Lane::Unknown, None))
}

/// What a job is: a cache hit, an attach, or new.
fn classify(job: &Job, laptop: &Laptop, vm: Option<&PreflightReply>) -> Kind {
    if let Some(result) = laptop.cache.get(&job.id) {
        return Kind::CacheHit(result.clone());
    }
    if let Some(VmState::Cached { result }) = vm.map(|r| r.state(&job.id)) {
        return Kind::CacheHit(result.clone());
    }
    let record = laptop.records.get(&job.id);
    if laptop.locked.contains(&job.id) && record.is_some_and(|r| r.state == State::Laptop) {
        return Kind::AttachLaptop;
    }
    match vm {
        Some(r) if matches!(r.state(&job.id), VmState::Queued | VmState::Running) => Kind::AttachVm,
        Some(_) => Kind::New,
        // A `sent` record always holds its token, and `vm` and `moving` ones their run.
        None => match record {
            Some(r) if r.state == State::Sent => r
                .token
                .clone()
                .map_or(Kind::New, |t| Kind::AttachOffline(RunRef::Token(t))),
            Some(r) if matches!(r.state, State::Vm | State::Moving) => r
                .run
                .clone()
                .map_or(Kind::New, |run| Kind::AttachOffline(RunRef::Run(run))),
            _ => Kind::New,
        },
    }
}

/// The older versions of `id` under `key`, each with whether it runs. Laptop
/// jobs with a live supervisor; VM jobs as the preflight reports them, or, while
/// the VM is unreachable, every record that may be on it (counted as running);
/// and jobs holding a pending entry under the key.
fn older(id: &str, key: &Key, laptop: &Laptop, vm: Option<&PreflightReply>) -> Vec<(String, bool)> {
    let mut found: BTreeMap<String, bool> = BTreeMap::new();
    let mut add = |old: &str, running: bool| {
        if old != id {
            let r = found.entry(old.to_owned()).or_insert(false);
            *r = *r || running;
        }
    };
    for r in laptop.records.values() {
        let holds = |entries: &[Entry]| entries.iter().any(|e| &e.key == key);
        let pending = r.pending.iter().any(|p| &p.entry.key == key);
        match r.state {
            State::Laptop if laptop.locked.contains(&r.id) && holds(&r.entries) => {
                add(&r.id, laptop.running.contains(&r.id));
            }
            State::Vm | State::Sent | State::Moving
                if vm.is_none() && (holds(&r.mirror) || holds(&r.entries) || pending) =>
            {
                add(&r.id, true);
            }
            State::Vm | State::Sent | State::Moving if pending => {
                match vm.map(|v| v.state(&r.id)) {
                    Some(VmState::Running) => add(&r.id, true),
                    Some(VmState::Queued) => add(&r.id, false),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    for o in vm.map_or(&[][..], |r| r.older.as_slice()) {
        if &o.key == key {
            add(&o.id, o.running);
        }
    }
    found.into_iter().collect()
}

/// Hand out the laptop's free slots once, in submit order, unknown jobs first,
/// then short ones, and route every job.
fn route(jobs: &mut [Planned], mut free: usize, reachable: bool, local: bool) {
    for j in jobs.iter_mut() {
        j.route = match &j.kind {
            Kind::CacheHit(_) => Route::None,
            Kind::AttachLaptop => Route::LaptopAttach,
            Kind::AttachVm => Route::VmAttach,
            Kind::AttachOffline(_) => Route::Pending,
            Kind::New if local || !reachable => Route::Laptop,
            Kind::New if j.lane >= Lane::Long || j.vm_only => Route::VmQueue,
            Kind::New => continue,
        };
    }
    for lane in [Lane::Unknown, Lane::Short] {
        for j in jobs
            .iter_mut()
            .filter(|j| j.kind == Kind::New && j.lane == lane && j.route == Route::None)
        {
            let slot = free > 0;
            free -= usize::from(slot);
            j.route = match (lane, slot) {
                (Lane::Unknown, true) => Route::Laptop,
                (Lane::Unknown, false) => Route::VmQueue,
                (_, true) => Route::VmNowOrRefuse,
                (_, false) => Route::VmNowOrQueue,
            };
        }
    }
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;

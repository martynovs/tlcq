//! The runner's state (design D9, D10): every run of every id the VM holds, the
//! queue, commit tokens and the cache, kept as one document that each verb reads
//! and rewrites whole under the admission lock.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::store::CancelMarker;
use crate::engine::Engine;
use crate::entry::Entry;
use crate::outcome::{JobResult, LaneDecision, Outcome};
use crate::proto::{
    CommitAnswer, Counters, Held, JobInfo, PreflightReply, PreflightRequest, VmJob, VmOlder,
    VmState,
};

/// Where a run is in its life on the VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Phase {
    /// In the queue.
    Queued,
    /// Admitted: an attempt runs or is frozen, or the run is paused, or waits
    /// first in the queue for its resume.
    Running {
        /// Its place: `vm` and the slot.
        place: String,
        /// Seconds it has run.
        elapsed_secs: u64,
        /// TLC's latest counters.
        counters: Option<Counters>,
    },
    /// Cancelled, its container still stopping.
    Stopping {
        /// Who cancelled it.
        by: String,
        /// The id that replaced it, for a supersede.
        replaced_by: Option<String>,
    },
    /// Ended.
    Ended {
        /// Its result.
        result: JobResult,
    },
}

/// One run of an id on the VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// Its name: `v<n>`, or the laptop name a moved run keeps.
    pub name: String,
    /// Its phase.
    pub phase: Phase,
    /// Its submitters.
    pub entries: Vec<Entry>,
    /// The job's description, as the commit that queued it gave it.
    pub info: JobInfo,
    /// Its recorded VM attempts, oldest first (D10).
    pub attempts: Vec<Attempt>,
    /// The number of its last attempt, recorded or not: numbers are never reused.
    pub last_attempt: u32,
    /// How many attempts in a row were undone, gone before their started marker.
    pub undone: u32,
    /// Whether a resume is queued: its next attempt starts first, recovering
    /// the checkpoint. (A queued restart from before pinned slots reads as one.)
    #[serde(alias = "restart")]
    pub resume: bool,
    /// The run's duration so far, as its last settled attempt left it.
    #[serde(default)]
    pub duration_secs: u64,
    /// The run's lane decisions so far, as its last settled attempt left them.
    #[serde(default)]
    pub lanes: Vec<LaneDecision>,
    /// When it ended, in seconds since the epoch: however it ended, attempts or
    /// none (the page lists ended runs by it).
    #[serde(default)]
    pub ended_at: Option<u64>,
}

/// A VM slot, pinned to its CPUs (pinned-slots S1). An attempt recorded before
/// pinned slots reads its share as one: the 70% slot and 100% as E, the side
/// slot as P.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    /// The efficiency cores: filled first, never closed.
    #[serde(alias = "full", alias = "main")]
    E,
    /// The performance cores: a human may close it.
    #[serde(alias = "side")]
    P,
}

impl Slot {
    /// Both slots, in the order admission fills them.
    pub const ALL: [Slot; 2] = [Slot::E, Slot::P];

    /// The place reports show.
    pub fn place(self) -> &'static str {
        match self {
            Slot::E => "vm E",
            Slot::P => "vm P",
        }
    }
}

/// Where a live attempt stands with freezing (pinned-slots S4), as the page
/// and admission left it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freeze {
    /// Running in its slot.
    #[default]
    Running,
    /// Its container paused: no CPU, its memory kept, no slot held.
    Frozen,
    /// Frozen, running on spare capacity in its slot: frozen again when new
    /// work needs the slot.
    Thawed,
    /// Frozen and unfrozen: it takes the next free slot before the queue.
    Unfrozen,
    /// Frozen, then unpaused for its pause: it stops at once, in no slot.
    Unpaused,
}

/// One recorded VM attempt of a run.
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per thing the runner records of an attempt"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    /// Its number, from 1.
    pub number: u32,
    /// Its slot: the one it started in, or last thawed into.
    #[serde(alias = "share")]
    pub slot: Slot,
    /// Whether its end is recorded: its container settled.
    pub ended: bool,
    /// Whether it continues a resume: undone, it returns to the queued resume.
    #[serde(alias = "restart")]
    pub resume: bool,
    /// Where it stands with freezing.
    #[serde(default)]
    pub freeze: Freeze,
    /// When it was last frozen, in seconds since the epoch.
    #[serde(default)]
    pub frozen_at: Option<u64>,
    /// Whether a pause is asked for: its supervisor stops it at a checkpoint.
    #[serde(default)]
    pub pausing: bool,
    /// Whether it ended for a pause: its run is paused, its checkpoint kept.
    #[serde(default)]
    pub paused: bool,
    /// Why it stopped, when it died and left the run paused on a checkpoint:
    /// a long run's, or the one its resume recovered.
    #[serde(default)]
    pub died: Option<String>,
    /// The memory its container started with, in MB; none for one recorded
    /// before attempts kept it.
    #[serde(default)]
    pub memory_mb: Option<u64>,
    /// Whether its run asks for more memory than one slot has, so it holds
    /// the whole VM: the P slot starts and thaws nothing while it is live
    /// (job-memory M3).
    #[serde(default)]
    pub whole: bool,
    /// Whether a human froze it: it stays frozen, never thawed on spare
    /// capacity, until an unfreeze or its pause.
    #[serde(default)]
    pub held: bool,
}

impl Attempt {
    /// A new attempt `number` in `slot`.
    pub fn new(number: u32, slot: Slot, resume: bool) -> Self {
        Self {
            number,
            slot,
            ended: false,
            resume,
            freeze: Freeze::Running,
            frozen_at: None,
            pausing: false,
            paused: false,
            died: None,
            memory_mb: None,
            whole: false,
            held: false,
        }
    }

    /// The slot it holds: none once ended, frozen or unpaused for its pause.
    pub fn holds(&self) -> Option<Slot> {
        (!self.ended && matches!(self.freeze, Freeze::Running | Freeze::Thawed))
            .then_some(self.slot)
    }
}

impl Run {
    /// Whether it is queued or running (frozen, paused, or waiting for its
    /// resume included): a run entries can attach to.
    pub fn live(&self) -> bool {
        matches!(self.phase, Phase::Queued | Phase::Running { .. })
    }

    /// Its attempt with no end recorded, if any.
    pub fn live_attempt(&self) -> Option<&Attempt> {
        self.attempts.last().filter(|a| !a.ended)
    }

    /// Whether it is paused: its last attempt ended for a pause, and no resume
    /// is queued.
    pub fn paused(&self) -> bool {
        matches!(self.phase, Phase::Running { .. })
            && !self.resume
            && self.attempts.last().is_some_and(|a| a.ended && a.paused)
    }

    /// How the page holds it, as the laptop sees it: frozen, pausing, paused,
    /// or resumed or unfrozen and waiting first for a slot.
    pub fn held(&self) -> Option<Held> {
        if self.paused() {
            let died = self.attempts.last().and_then(|a| a.died.clone());
            return Some(Held::Paused {
                at: None,
                died,
                checkpoint_mb: None,
            });
        }
        let Some(a) = self.live_attempt() else {
            return (matches!(self.phase, Phase::Running { .. }) && self.resume)
                .then_some(Held::Waiting);
        };
        match a.freeze {
            _ if a.pausing => Some(Held::Pausing),
            Freeze::Unfrozen => Some(Held::Waiting),
            Freeze::Frozen => Some(Held::Frozen {
                since: a.frozen_at,
                spare: false,
            }),
            Freeze::Thawed => Some(Held::Frozen {
                since: a.frozen_at,
                spare: true,
            }),
            Freeze::Running | Freeze::Unpaused => None,
        }
    }
}

/// The runner's whole state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Every id's runs, oldest first.
    pub runs: BTreeMap<String, Vec<Run>>,
    /// The number of each id's last `v<n>`.
    pub numbered: BTreeMap<String, u32>,
    /// Live runs, in admission order: first in, first out unless a human
    /// reorders. A run keeps its place while it runs, so an undone attempt's run
    /// waits where it was.
    pub queue: Vec<(String, String)>,
    /// What each commit, by token, applied; a tombstone is a `NotApplied` no commit wrote.
    pub tokens: BTreeMap<String, CommitAnswer>,
    /// The VM's cache: results of its own runs, `ok` and `unexpected` only.
    pub cache: BTreeMap<String, JobResult>,
    /// Whether the last admission round stopped because the VM had too little
    /// memory free for the next attempt (what a queued job waits for).
    #[serde(default)]
    pub short_of_memory: bool,
}

impl State {
    /// The latest run of `id`.
    pub fn latest(&self, id: &str) -> Option<&Run> {
        self.runs.get(id).and_then(|r| r.last())
    }

    /// A job's state as the laptop sees it: its live run if any, else its cached
    /// result, else its latest run's end, else unknown.
    pub fn vm_job(&self, id: &str) -> VmJob {
        let run = self.latest(id);
        VmJob {
            position: run.and_then(|r| self.position(id, &r.name)),
            ..view(run, self.cache.get(id))
        }
    }

    /// The place of run `name` of `id` among the queued runs, from 1.
    fn position(&self, id: &str, name: &str) -> Option<usize> {
        let queued = |(i, n): &&(String, String)| {
            self.runs
                .get(i)
                .and_then(|rs| rs.iter().find(|r| &r.name == n))
                .is_some_and(|r| r.phase == Phase::Queued)
        };
        let p = self
            .queue
            .iter()
            .filter(queued)
            .position(|(i, n)| i == id && n == name)?;
        Some(p + 1)
    }

    /// The state of the run `name` of `id`, cache aside.
    pub fn run_job(&self, id: &str, name: &str) -> VmJob {
        let run = self
            .runs
            .get(id)
            .and_then(|rs| rs.iter().find(|r| r.name == name));
        VmJob {
            position: self.position(id, name),
            ..view(run, None)
        }
    }

    /// The preflight (D6): read-only but for token tombstones. Every token it is
    /// asked about that no commit applied is tombstoned, so a commit carrying it
    /// that arrives later applies nothing, and the answer is final.
    pub fn preflight(
        &mut self,
        req: &PreflightRequest,
        versions: &crate::engine::Versions,
    ) -> PreflightReply {
        let jobs = req
            .ids
            .iter()
            .map(|id| (id.clone(), self.vm_job(id)))
            .collect();
        let tokens = req
            .tokens
            .iter()
            .map(|t| {
                let answer = self
                    .tokens
                    .entry(t.clone())
                    .or_insert(CommitAnswer::NotApplied);
                (t.clone(), answer.clone())
            })
            .collect();
        let keys: BTreeSet<_> = req.keys.iter().collect();
        let mut older = Vec::new();
        for (id, runs) in &self.runs {
            for run in runs.iter().filter(|r| r.live()) {
                for e in run.entries.iter().filter(|e| keys.contains(&e.key)) {
                    let job = self.vm_job(id);
                    older.push(VmOlder {
                        id: id.clone(),
                        key: e.key.clone(),
                        running: job.state == VmState::Running,
                        place: job.place.unwrap_or_else(|| "vm".to_owned()),
                        elapsed_secs: job.elapsed_secs.unwrap_or(0),
                        counters: job.counters,
                    });
                }
            }
        }
        let runs = req
            .runs
            .iter()
            .map(|r| (r.clone(), self.run_job(&r.id, &r.run)))
            .collect();
        PreflightReply {
            runs,
            jars_match: req.versions.matches(versions, Engine::Tlc),
            tlx_match: Some(req.versions.matches(versions, Engine::Tlx)),
            jobs,
            tokens,
            older,
            disk: None,
        }
    }

    /// End a run `name` of `id` with `result`, caching it if its outcome is cached.
    pub fn end(&mut self, id: &str, name: &str, result: JobResult) {
        if result.outcome.cached() {
            self.cache
                .entry(id.to_owned())
                .or_insert_with(|| result.clone());
        }
        self.queue.retain(|(i, n)| !(i == id && n == name));
        if let Some(run) = self
            .runs
            .get_mut(id)
            .and_then(|runs| runs.iter_mut().find(|r| r.name == name))
        {
            run.phase = Phase::Ended { result };
            run.ended_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs());
        }
    }

    /// Cancel the live run `name` of `id` (D10). With no live attempt (queued,
    /// paused, or a resume pending) it ends `cancelled` at once, dropping any
    /// queued resume; else it is stopping, and the caller writes `marker` in
    /// the returned attempt and stops its container.
    pub fn cancel_run(&mut self, id: &str, name: &str, marker: &CancelMarker) -> Option<u32> {
        let run = self
            .runs
            .get_mut(id)?
            .iter_mut()
            .find(|r| r.name == name && r.live())?;
        if let Some(a) = run.attempts.last().filter(|a| !a.ended) {
            run.phase = Phase::Stopping {
                by: marker.by.clone(),
                replaced_by: marker.replaced_by.clone(),
            };
            return Some(a.number);
        }
        run.resume = false;
        // With no live attempt, the run so far is what its last one left.
        let result = JobResult {
            duration_secs: run.duration_secs,
            lanes: run.lanes.clone(),
            ..State::outcome_only(Outcome::Cancelled {
                by: marker.by.clone(),
                replaced_by: marker.replaced_by.clone(),
            })
        };
        self.end(id, name, result);
        None
    }

    /// Resume the paused run `name` of `id` (S5): its next attempt goes first
    /// in the queue and recovers the checkpoint, asking for `memory_mb` and
    /// capped at `timeout_secs` when given. Returns whether it was paused.
    pub fn resume(
        &mut self,
        id: &str,
        name: &str,
        (memory_mb, timeout_secs): (Option<u64>, Option<u64>),
    ) -> bool {
        let Some(run) = self
            .runs
            .get_mut(id)
            .and_then(|rs| rs.iter_mut().find(|r| r.name == name))
            .filter(|r| r.paused())
        else {
            return false;
        };
        run.resume = true;
        if memory_mb.is_some() {
            run.info.memory_mb = memory_mb;
        }
        if timeout_secs.is_some() {
            run.info.timeout_secs = timeout_secs;
        }
        let entry = (id.to_owned(), name.to_owned());
        self.queue.retain(|e| *e != entry);
        self.queue.insert(0, entry);
        true
    }

    /// A result that is only an outcome, for runs that ended without TLC's word.
    pub fn outcome_only(outcome: Outcome) -> JobResult {
        JobResult {
            outcome,
            exit_status: None,
            distinct: None,
            generated: None,
            depth: None,
            place: "vm".to_owned(),
            duration_secs: 0,
            lanes: Vec::new(),
            errors: Vec::new(),
            note: None,
        }
    }
}

/// A run as the laptop sees it: a cached result outranks its end.
fn view(run: Option<&Run>, cached: Option<&JobResult>) -> VmJob {
    let state = match run.map(|r| &r.phase) {
        Some(Phase::Queued) => VmState::Queued,
        Some(Phase::Running { .. }) => VmState::Running,
        Some(Phase::Stopping { .. }) => VmState::Stopping,
        ended => match (cached, ended) {
            (Some(result), _) => VmState::Cached {
                result: result.clone(),
            },
            (None, Some(Phase::Ended { result })) => VmState::Ended {
                result: result.clone(),
            },
            _ => VmState::Unknown,
        },
    };
    let (place, elapsed_secs, counters) = match run.map(|r| &r.phase) {
        Some(Phase::Running {
            place,
            elapsed_secs,
            counters,
        }) => {
            // A slot named only while it holds one: not frozen, paused or waiting.
            let holds = run.is_some_and(|r| r.live_attempt().is_some_and(|a| a.holds().is_some()));
            (holds.then(|| place.clone()), Some(*elapsed_secs), *counters)
        }
        _ => (None, None, None),
    };
    VmJob {
        state,
        run: run.map(|r| r.name.clone()),
        entries: run.map(|r| r.entries.clone()).unwrap_or_default(),
        place,
        elapsed_secs,
        counters,
        lanes: Vec::new(),
        flags: run.map(|r| r.info.flags()).unwrap_or_default(),
        curve: None,
        position: None,
        left_secs: None,
        cancelled: match run.map(|r| &r.phase) {
            Some(Phase::Stopping { by, replaced_by }) => Some((by.clone(), replaced_by.clone())),
            _ => None,
        },
        started_at: None,
        expected: run.and_then(|r| Some((r.info.expected?, r.info.source))),
        waits_for: None,
        held: run.and_then(Run::held),
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
pub(crate) mod tests;

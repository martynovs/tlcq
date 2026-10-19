//! What the laptop and the runner exchange (design D6): the preflight's request
//! and reply. Each travels as JSON on the runner command's stdin and stdout.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::engine::{Engine, Versions};
use crate::entry::{Entry, Key};
use crate::history::{Expected, Keys, Source};
use crate::outcome::{JobResult, LaneDecision};

/// What a pending entry or a follow refers to: a run, or a commit's token
/// until a refresh resolves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunRef {
    /// A run name, `l<n>` or `v<n>`.
    Run(String),
    /// A commit's token.
    Token(String),
}

/// The preflight's request: read-only on the VM, but for token tombstones.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightRequest {
    /// The laptop's engine versions.
    #[serde(flatten)]
    pub versions: Versions,
    /// The batch's ids, and every id the laptop holds as `vm`, `sent` or lock-free `moving`.
    pub ids: Vec<String>,
    /// The batch's keys, for the older versions the VM holds.
    pub keys: Vec<Key>,
    /// Tokens of records and follows still holding one.
    pub tokens: Vec<String>,
    /// Runs to report on their own (`status`): a follow's run, which may be
    /// older than its id's latest.
    #[serde(default)]
    pub runs: Vec<RunAsk>,
}

/// A run asked about by name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RunAsk {
    /// The job.
    pub id: String,
    /// The run.
    pub run: String,
}

/// `cancel`'s request: end a VM job's run, queued or running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelRequest {
    /// The job.
    pub id: String,
    /// The run, if the laptop knows it; else the id's live run.
    pub run: Option<String>,
    /// The canceller's label.
    pub by: String,
}

/// `resume`'s request: resume a VM job's paused run from its checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeRequest {
    /// The job.
    pub id: String,
    /// The run, if the laptop knows it; else the id's live run.
    pub run: Option<String>,
    /// The memory its attempts now ask for, in MB, when given.
    pub memory_mb: Option<u64>,
    /// Its new time cap, in seconds of the run's VM time, when given.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// `resume`'s reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeReply {
    /// The run's state after.
    pub job: VmJob,
    /// Why it was not resumed, if it was not.
    pub refused: Option<String>,
}

/// `fetch`'s request: the files of a run of a VM job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchRequest {
    /// The job.
    pub id: String,
    /// The run the agent follows; else the id's latest run.
    pub run: Option<String>,
}

/// One file `fetch` streams after its reply line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchFile {
    /// The attempt it belongs to.
    pub attempt: u32,
    /// Its name in the attempt's directory.
    pub name: String,
    /// Its length: exactly this many bytes follow for it.
    pub len: u64,
}

/// `fetch`'s reply line; the files' bytes follow it, in its order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchReply {
    /// The run's state.
    pub job: VmJob,
    /// Its recorded attempts' files.
    pub files: Vec<FetchFile>,
}

/// A job's state on the VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum VmState {
    /// The VM holds no run of this id.
    Unknown,
    /// Waiting for a slot (a run frozen, paused or with a resume pending counts as running).
    Queued,
    /// Running, frozen or paused, or waiting for its resume.
    Running,
    /// Cancelled, its container still stopping: it cannot be attached to.
    Stopping,
    /// The VM's cache holds a result for this id.
    Cached {
        /// The result.
        result: JobResult,
    },
    /// Its latest run ended, uncached.
    Ended {
        /// The result.
        result: JobResult,
    },
}

/// A job as the VM reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmJob {
    /// Its state.
    #[serde(flatten)]
    pub state: VmState,
    /// Its latest run's name.
    pub run: Option<String>,
    /// Its entries.
    pub entries: Vec<Entry>,
    /// Its place (`vm` and the slot), while queued or running.
    #[serde(default)]
    pub place: Option<String>,
    /// How long its run has run, in seconds, while running.
    #[serde(default)]
    pub elapsed_secs: Option<u64>,
    /// TLC's latest counters, while running.
    #[serde(default)]
    pub counters: Option<Counters>,
    /// The run's lane decisions so far, while running (D9): `status` reports
    /// each at the poll after it is made.
    #[serde(default)]
    pub lanes: Vec<LaneDecision>,
    /// The run's flags, its first submitter's: an attach keeps them (D6).
    #[serde(default)]
    pub flags: Flags,
    /// The progress curve of the run's last attempt, for the ETA (D15).
    #[serde(default)]
    pub curve: Option<AttemptCurve>,
    /// While queued, its place among the queued runs, from 1.
    #[serde(default)]
    pub position: Option<usize>,
    /// While running, the seconds left before its cap, and the cap.
    #[serde(default)]
    pub left_secs: Option<(u64, u64)>,
    /// While stopping, who cancelled it, and the id that replaced it.
    #[serde(default)]
    pub cancelled: Option<(String, Option<String>)>,
    /// While running, when its attempt started, in seconds since the epoch.
    #[serde(default)]
    pub started_at: Option<u64>,
    /// The duration it was expected to take, and where that came from: its
    /// lane until a lane decision.
    #[serde(default)]
    pub expected: Option<(crate::history::Expected, Option<crate::history::Source>)>,
    /// While queued, what it waits for: `the queue to be released`, `disk`,
    /// `memory` or `a VM slot`.
    #[serde(default)]
    pub waits_for: Option<String>,
    /// While running, how the page holds it (pinned-slots): frozen, pausing or
    /// paused. A laptop that predates this reads such a run as running.
    #[serde(default)]
    pub held: Option<Held>,
}

/// How the runner's page holds a running VM run (pinned-slots S4, S5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "held", rename_all = "snake_case")]
pub enum Held {
    /// Frozen: its container paused.
    Frozen {
        /// Since when, in seconds since the epoch.
        since: Option<u64>,
        /// Whether it runs on spare capacity now, until new work needs its slot.
        spare: bool,
    },
    /// Pausing: it stops at its engine's next checkpoint.
    Pausing,
    /// Paused at a checkpoint, until a resume.
    Paused {
        /// When it stopped, at its checkpoint, in seconds since the epoch.
        at: Option<u64>,
        /// Why it died, when that left it paused on its checkpoint (a long
        /// run's, or its resume's): it needs a resume, with more memory say,
        /// or a cancel to drop it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        died: Option<String>,
        /// The disk its kept checkpoint holds, in MB.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint_mb: Option<u64>,
    },
    /// Resumed or unfrozen: first in the queue, waiting for a slot.
    Waiting,
    /// A hold this laptop does not know (a newer runner's).
    #[serde(other)]
    Other,
}

impl Held {
    /// Whether its engine runs: on spare capacity, or pausing in its slot.
    pub fn runs(&self) -> bool {
        matches!(self, Held::Frozen { spare: true, .. } | Held::Pausing)
    }
}

/// One attempt's progress curve, as its file's text (`curve::Curve`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptCurve {
    /// The attempt.
    pub attempt: u32,
    /// The curve file's text: JSONL, or an older attempt's TSV.
    pub text: String,
}

/// The flags a job keeps from its first submitter (D6, Records).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flags {
    /// `--timeout`, in seconds.
    pub timeout_secs: Option<u64>,
    /// `--vm-on-timeout`.
    pub vm_on_timeout: bool,
    /// `--memory`, in MB; none from an older peer.
    #[serde(default)]
    pub memory_mb: Option<u64>,
}

impl JobInfo {
    /// The flags an attach keeps from this, the first submitter's description.
    pub fn flags(&self) -> Flags {
        Flags {
            timeout_secs: self.timeout_secs,
            vm_on_timeout: self.vm_on_timeout,
            memory_mb: self.memory_mb,
        }
    }
}

/// A job's description, `job.json` (D2): what travels beside its tarball, and
/// what the laptop keeps in its record, so a supervisor can run it and the
/// history can be written when it ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobInfo {
    /// The submitter entries it carries: the placing command's, or for a move
    /// every entry of the laptop job (D7).
    pub entries: Vec<Entry>,
    /// For a move, the laptop run's name, which the run keeps on the VM (D10).
    pub run: Option<String>,
    /// The spec module.
    pub spec: String,
    /// The engine it runs on; TLC for a description stored before tlx.
    #[serde(default = "Engine::stored")]
    pub engine: Engine,
    /// The allowlisted TLC options.
    pub tlc_opts: Vec<String>,
    /// The history keys, one per configuration name.
    pub names: Vec<Keys>,
    /// The expected duration, as the command found it (`None`: unknown).
    pub expected: Option<Expected>,
    /// Where that expectation came from (`configuration` is reported "from an earlier spec").
    pub source: Option<Source>,
    /// A time cap replacing every lane cap, in seconds (`--timeout`).
    pub timeout_secs: Option<u64>,
    /// `--local`: on the laptop, uncapped.
    pub local: bool,
    /// `--vm-on-timeout`.
    pub vm_on_timeout: bool,
    /// The least memory of its VM attempts, in MB (`--memory`; job-memory M1).
    #[serde(default)]
    pub memory_mb: Option<u64>,
    /// The laptop run's awake time so far, for a move (D10).
    pub laptop_secs: u64,
}

/// What became of a commit, by its token. One token covers every job of a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "commit", rename_all = "snake_case")]
pub enum CommitAnswer {
    /// The commit applied nothing, and never will: the lookup tombstoned it.
    NotApplied,
    /// The commit applied: what each of its jobs reached, by id.
    Applied {
        /// Each job's answer.
        jobs: BTreeMap<String, TokenAnswer>,
    },
}

/// What became of one job of a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "token", rename_all = "snake_case")]
pub enum TokenAnswer {
    /// The job reached this run.
    Reached {
        /// The run.
        run: String,
    },
    /// The job was answered from the cache.
    Cached {
        /// The result.
        result: JobResult,
    },
    /// The job was not applied: the commit applied nothing, or did not store it
    /// (a `now-or-refuse` job it refused).
    NotApplied,
}

/// A VM job holding an entry under one of the batch's keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmOlder {
    /// The job's id.
    pub id: String,
    /// The key it holds.
    pub key: Key,
    /// Whether it runs (frozen, paused or waiting for its resume included); else it is queued.
    pub running: bool,
    /// Its place: `vm`, and the slot.
    pub place: String,
    /// How long it has run, in seconds.
    pub elapsed_secs: u64,
    /// TLC's latest counters, once it has printed any.
    pub counters: Option<Counters>,
}

/// TLC's counters from a progress line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    /// States generated.
    pub generated: u64,
    /// Distinct states.
    pub distinct: u64,
    /// States left on the queue.
    pub queue: u64,
}

/// The preflight's reply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightReply {
    /// Whether the VM's jar version matches the laptop's.
    pub jars_match: bool,
    /// Whether the VM's tlx rev matches the laptop's, both having tlx; `None`
    /// from a runner older than engines, whose ids the laptop's never match.
    #[serde(default)]
    pub tlx_match: Option<bool>,
    /// Each id asked about.
    pub jobs: BTreeMap<String, VmJob>,
    /// Each token asked about.
    pub tokens: BTreeMap<String, CommitAnswer>,
    /// Queued or running VM jobs holding one of the batch's keys.
    pub older: Vec<VmOlder>,
    /// Each run asked about, in the order asked.
    #[serde(default)]
    pub runs: Vec<(RunAsk, VmJob)>,
    /// The state volume's free space; none from an older runner.
    #[serde(default)]
    pub disk: Option<Disk>,
}

/// The VM's state volume: what is free, and the floor below which no job starts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disk {
    /// Free, in MB.
    pub free_mb: u64,
    /// The floor, in MB.
    pub floor_mb: u64,
}

impl Disk {
    /// Below twice the floor: worth a warning, and the runner cleans up.
    pub fn low(&self) -> bool {
        self.free_mb < self.floor_mb.saturating_mul(2)
    }
}

impl PreflightReply {
    /// What the commit holding `token` did with the job `id`, if the VM answered
    /// for that token.
    pub fn token(&self, token: &str, id: &str) -> Option<TokenAnswer> {
        match self.tokens.get(token)? {
            CommitAnswer::NotApplied => Some(TokenAnswer::NotApplied),
            CommitAnswer::Applied { jobs } => {
                Some(jobs.get(id).cloned().unwrap_or(TokenAnswer::NotApplied))
            }
        }
    }

    /// The VM's state of `id`; an id it did not report is unknown.
    pub fn state(&self, id: &str) -> &VmState {
        self.jobs.get(id).map_or(&VmState::Unknown, |j| &j.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_round_trips_and_unreported_ids_are_unknown() {
        let mut reply = PreflightReply {
            jars_match: true,
            ..PreflightReply::default()
        };
        reply.jobs.insert(
            "a".to_owned(),
            VmJob {
                state: VmState::Queued,
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
        reply
            .tokens
            .insert("t".to_owned(), CommitAnswer::NotApplied);
        let mut jobs = BTreeMap::new();
        jobs.insert(
            "x".to_owned(),
            TokenAnswer::Reached {
                run: "v3".to_owned(),
            },
        );
        reply
            .tokens
            .insert("u".to_owned(), CommitAnswer::Applied { jobs });
        let json = serde_json::to_string(&reply).unwrap();
        assert_eq!(
            serde_json::from_str::<PreflightReply>(&json).unwrap(),
            reply
        );
        assert_eq!(reply.state("a"), &VmState::Queued);
        assert_eq!(reply.state("b"), &VmState::Unknown);
        // One token, a job per id: each resolves to its own run.
        assert_eq!(
            reply.token("u", "x"),
            Some(TokenAnswer::Reached {
                run: "v3".to_owned()
            })
        );
        assert_eq!(reply.token("u", "y"), Some(TokenAnswer::NotApplied));
        assert_eq!(reply.token("t", "x"), Some(TokenAnswer::NotApplied));
        assert_eq!(reply.token("w", "x"), None);
    }

    #[test]
    fn a_description_stored_before_tlx_reads_as_tlc() {
        let mut json = serde_json::to_value(crate::runner::state::tests::info()).unwrap();
        json.as_object_mut().unwrap().remove("engine");
        let info: JobInfo = serde_json::from_value(json).unwrap();
        assert_eq!(info.engine, Engine::Tlc);
        // A request from a laptop without tlx carries no rev.
        let req: PreflightRequest =
            serde_json::from_str(r#"{"jar_version":"J","ids":[],"keys":[],"tokens":[]}"#).unwrap();
        assert_eq!(req.versions.tlx_rev, None);
        assert_eq!(req.versions.jar_version, "J");
    }
}

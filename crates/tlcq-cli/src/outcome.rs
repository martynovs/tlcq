//! A run's outcome and the result the caches keep (design D8, job-status spec).

use serde::{Deserialize, Serialize};

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// TLC ended as the configuration's header expects.
    Ok,
    /// TLC ended otherwise, or with a deterministic evaluation, parse or configuration error.
    Unexpected,
    /// The run passed its time cap.
    Timeout,
    /// Someone cancelled it.
    Cancelled {
        /// The canceller's label (`ui` for the web page).
        by: String,
        /// The id that replaced it, for a supersede.
        replaced_by: Option<String>,
    },
    /// Anything else: a crash, an out-of-memory kill, a job too big, a restart of the machine.
    Error {
        /// Why, when known.
        reason: Option<String>,
    },
    /// A commit whose outcome was unknown never reached the VM.
    NeverSent,
}

impl Outcome {
    /// Whether a result with this outcome is cached: only `ok` and `unexpected` are.
    pub fn cached(&self) -> bool {
        matches!(self, Outcome::Ok | Outcome::Unexpected)
    }

    /// The exit code `status` gives for it: 3 for a failure of the run, 1 for
    /// `unexpected`, 0 for `ok`.
    pub fn exit_code(&self) -> u8 {
        match self {
            Outcome::Ok => 0,
            Outcome::Unexpected => 1,
            _ => 3,
        }
    }
}

/// What the caches keep for an id, and what an ended record reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResult {
    /// How the run ended.
    #[serde(flatten)]
    pub outcome: Outcome,
    /// TLC's exit status, if it exited.
    pub exit_status: Option<i32>,
    /// Distinct states found.
    pub distinct: Option<u64>,
    /// States generated.
    pub generated: Option<u64>,
    /// The search depth.
    pub depth: Option<u64>,
    /// Where it ran: `laptop` or `vm`.
    pub place: String,
    /// Its duration, in seconds of awake time.
    pub duration_secs: u64,
    /// The lane decisions the VM's supervisor made (D9), in order.
    #[serde(default)]
    pub lanes: Vec<LaneDecision>,
    /// TLC's `Error:` lines (D8).
    #[serde(default)]
    pub errors: Vec<String>,
    /// A word on an `unexpected` verdict: the property TLC reported when the
    /// configuration expected another, and the `--must-fail-only` hint.
    #[serde(default)]
    pub note: Option<String>,
}

impl JobResult {
    /// Whether the VM judged the job max: then the history records max, whatever
    /// the run's duration (D5).
    pub fn judged_max(&self) -> bool {
        self.lanes.iter().any(|l| l.to == Lane::Max)
    }
}

/// The lane a decision moved a job to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lane {
    /// A short job still running at 15 minutes.
    Slow,
    /// A slow or unknown job the size probe judged big.
    Max,
}

/// One lane decision, with the probe's numbers if a probe made it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneDecision {
    /// The lane it moved to.
    pub to: Lane,
    /// When, in seconds of the run's VM time.
    pub after_secs: u64,
    /// The probe's numbers, for a probe's decision.
    pub probe: Option<Probe>,
}

/// The size probe's numbers (`tla/probe.sh`): the progress lines 5 minutes
/// before the probe and at it. `big` when queue / distinct ≥ 0.4, or the queue
/// grew by ≥ 0.1 of the distinct states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    /// Distinct states at the probe.
    pub distinct: u64,
    /// States left on the queue at the probe.
    pub queue: u64,
    /// States left on the queue 5 minutes before.
    pub queue_before: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_verdicts_are_cached() {
        assert!(Outcome::Ok.cached());
        assert!(Outcome::Unexpected.cached());
        assert!(!Outcome::Timeout.cached());
        assert!(!Outcome::NeverSent.cached());
    }

    #[test]
    fn exit_codes() {
        assert_eq!(Outcome::Ok.exit_code(), 0);
        assert_eq!(Outcome::Unexpected.exit_code(), 1);
        let cancelled = Outcome::Cancelled {
            by: "ui".to_owned(),
            replaced_by: None,
        };
        assert_eq!(cancelled.exit_code(), 3);
        assert_eq!(Outcome::Error { reason: None }.exit_code(), 3);
    }

    #[test]
    fn a_result_round_trips_as_flat_json() {
        let r = JobResult {
            outcome: Outcome::Cancelled {
                by: "ui".to_owned(),
                replaced_by: Some("abc".to_owned()),
            },
            exit_status: None,
            distinct: Some(5),
            generated: Some(9),
            depth: Some(3),
            place: "vm".to_owned(),
            duration_secs: 60,
            lanes: Vec::new(),
            errors: Vec::new(),
            note: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            json.starts_with("{\"outcome\":\"cancelled\",\"by\":\"ui\""),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<JobResult>(&json).unwrap(), r);
    }

    #[test]
    fn a_max_decision_makes_the_job_judged_max() {
        let json = r#"{"outcome":"error","reason":"too big","exit_status":null,"distinct":null,"generated":null,"depth":null,"place":"vm","duration_secs":900}"#;
        let mut r: JobResult = serde_json::from_str(json).unwrap();
        assert_eq!(r.lanes, Vec::new());
        assert!(!r.judged_max());
        r.lanes.push(LaneDecision {
            to: Lane::Slow,
            after_secs: 900,
            probe: None,
        });
        assert!(!r.judged_max());
        r.lanes.push(LaneDecision {
            to: Lane::Max,
            after_secs: 1800,
            probe: Some(Probe {
                distinct: 100,
                queue: 50,
                queue_before: 20,
            }),
        });
        assert!(r.judged_max());
    }
}

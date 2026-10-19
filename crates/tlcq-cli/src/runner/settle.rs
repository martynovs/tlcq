//! Settling an attempt whose container is gone (design D10): the stop hook, also
//! run by every writing verb and at the runner's start, so a missed hook is made
//! good and settling twice changes nothing.

use super::state::{Phase, State};
use crate::outcome::{JobResult, LaneDecision, Outcome};
use crate::tlc::Counts;

/// What a gone attempt's directory holds.
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per marker file, as the directory holds them"
)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// The supervisor's started marker.
    pub started: bool,
    /// A cancel marker: the canceller's label and any replacing id.
    pub cancel: Option<(String, Option<String>)>,
    /// The verdict in `result.json`, written only when TLC exited on its own.
    pub finished: Option<JobResult>,
    /// The supervisor's paused stop marker (never the page's request).
    pub paused: bool,
    /// Whether this attempt's checkpoint was kept for the run's resume.
    pub kept: bool,
    /// A resume's: the run's checkpoint, which an earlier attempt kept, is on disk.
    pub recovered: bool,
    /// Its engine's log says the recovery of that checkpoint completed.
    pub recovery_completed: bool,
    /// The signal that killed the engine, if one did.
    pub signal: Option<i32>,
    /// Whether the engine's log says it ran out of memory.
    pub out_of_memory: bool,
    /// The supervisor's stop of a frozen job paused before its first checkpoint.
    pub no_checkpoint: bool,
    /// A timed-out marker.
    pub timed_out: bool,
    /// The run's duration so far, from `result.json`.
    pub duration_secs: u64,
    /// The run's lane decisions so far, from `result.json`.
    pub lanes: Vec<LaneDecision>,
    /// TLC's last counters, from its log.
    pub counts: Option<Counts>,
}

/// What settling did, for the caller's files and containers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// Nothing: no such attempt, or it was settled already.
    Already,
    /// The attempt never started: its record is gone and the run waits again;
    /// delete its directory.
    Undone,
    /// The run ended; delete the attempt's scratch.
    Ended,
    /// The run is paused, its checkpoint kept; delete the attempt's scratch.
    Paused,
}

impl State {
    /// Settle attempt `number` of run `name` of `id`, gone, from what its
    /// directory holds. A run undone `limit` times in a row ends `error`; one
    /// killed for memory is told to ask for up to `most_mb`, the most one job
    /// gets.
    pub fn settle(
        &mut self,
        id: &str,
        name: &str,
        number: u32,
        seen: &Observed,
        (limit, most_mb): (u32, u64),
    ) -> Settled {
        let Some(run) = self
            .runs
            .get_mut(id)
            .and_then(|runs| runs.iter_mut().find(|r| r.name == name))
        else {
            return Settled::Already;
        };
        let Some(attempt) = run
            .attempts
            .last_mut()
            .filter(|a| a.number == number && !a.ended)
        else {
            return Settled::Already;
        };
        let place = attempt.slot.place();
        let counts = seen.counts;
        let result = |outcome| JobResult {
            outcome,
            duration_secs: seen.duration_secs,
            lanes: seen.lanes.clone(),
            distinct: counts.map(|c| c.distinct),
            generated: counts.map(|c| c.generated),
            depth: counts.map(|c| c.depth),
            place: place.to_owned(),
            ..State::outcome_only(Outcome::Ok)
        };
        if !seen.started && seen.cancel.is_none() {
            let resume = attempt.resume;
            run.attempts.pop();
            run.undone += 1;
            if run.undone >= limit {
                run.resume = false;
                let never = result(Outcome::Error {
                    reason: Some("never started".to_owned()),
                });
                self.end(id, name, never);
            } else if resume {
                run.resume = true;
            } else {
                run.phase = Phase::Queued;
            }
            return Settled::Undone;
        }
        attempt.ended = true;
        if seen.started {
            run.undone = 0;
        }
        run.duration_secs = seen.duration_secs;
        run.lanes.clone_from(&seen.lanes);
        let own_pause =
            seen.cancel.is_none() && seen.finished.is_none() && seen.paused && seen.kept;
        // Killed for memory: what it had, which a rerun needs more than.
        let hint = attempt
            .memory_mb
            .filter(|_| seen.out_of_memory || seen.signal == Some(9))
            .map(|mb| super::memory::killed_hint(mb, most_mb));
        let again = paused_again(seen, hint.as_deref());
        if own_pause || again.is_some() {
            attempt.paused = true;
            attempt.died = again;
            run.phase = Phase::Running {
                place: place.to_owned(),
                elapsed_secs: seen.duration_secs,
                counters: None,
            };
            return Settled::Paused;
        }
        let result = match (&seen.cancel, &seen.finished) {
            (Some((by, replaced_by)), _) => result(Outcome::Cancelled {
                by: by.clone(),
                replaced_by: replaced_by.clone(),
            }),
            (None, Some(finished)) => finished.clone(),
            (None, None) if seen.no_checkpoint => JobResult {
                note: Some("paused before its first checkpoint".to_owned()),
                ..result(Outcome::Cancelled {
                    by: "ui".to_owned(),
                    replaced_by: None,
                })
            },
            (None, None) if seen.timed_out => result(Outcome::Timeout),
            (None, None) if seen.paused => result(Outcome::Error {
                reason: Some("stopped for a pause before its checkpoint was kept".to_owned()),
            }),
            (None, None) => JobResult {
                note: hint,
                ..result(Outcome::Error {
                    reason: Some(died(seen)),
                })
            },
        };
        self.end(id, name, result);
        Settled::Ended
    }
}

/// Why an attempt leaves its run paused on a checkpoint to recover, ranked as
/// settling ranks the rest (a cancel first, then the engine's verdict, then
/// the markers), `hint` added for one killed for memory: a long run died
/// with no verdict or an error, or stopped at its cap, its checkpoint kept
/// for the agent to resume or drop; or a resume's engine refused the checkpoint
/// before recovering it (a tlx build that did not write it, say), or died
/// with nothing to say, for another resume (with more memory, say).
fn paused_again(seen: &Observed, hint: Option<&str>) -> Option<String> {
    let with_hint = |why: String| match hint {
        Some(h) => format!("{why}; {h}"),
        None => why,
    };
    if seen.cancel.is_some() {
        return None;
    }
    if seen.kept && !seen.paused {
        return match &seen.finished {
            None if seen.timed_out => {
                Some("stopped at its time cap: resume it with a longer --timeout".to_owned())
            }
            None => Some(with_hint(died(seen))),
            Some(JobResult {
                outcome: Outcome::Error { reason },
                note,
                ..
            }) => {
                let why = reason.clone().unwrap_or_else(|| "error".to_owned());
                Some(
                    note.as_ref()
                        .map_or_else(|| why.clone(), |n| format!("{why}; {n}")),
                )
            }
            Some(_) => None,
        };
    }
    if !seen.recovered {
        return None;
    }
    match &seen.finished {
        Some(f) if !seen.recovery_completed => Some(match &f.outcome {
            Outcome::Error { reason: Some(r) } => r.clone(),
            _ => "its engine exited before recovering the checkpoint".to_owned(),
        }),
        Some(_) => None,
        None if (seen.paused && seen.kept) || seen.no_checkpoint || seen.timed_out => None,
        None => Some(with_hint(died(seen))),
    }
}

/// Why an attempt stopped with no verdict and no marker: the signal that
/// killed its engine, if known (a SIGKILL most likely the kernel's, for memory).
fn died(seen: &Observed) -> String {
    match (seen.signal, seen.out_of_memory) {
        (_, true) => "stopped: out of memory".to_owned(),
        (Some(9), false) => "stopped: killed (signal 9), most likely out of memory".to_owned(),
        (Some(s), false) => format!("stopped: killed by signal {s}"),
        (None, false) => "stopped: a shutdown or out of memory".to_owned(),
    }
}

#[cfg(test)]
#[path = "settle_tests.rs"]
mod tests;

//! The VM supervisor's lane decisions and caps (design D9, pinned-slots S3), as
//! a state machine fed with TLC's progress and the clock; the supervisor
//! carries out what it returns.
//!
//! Lane timers and caps count the run's VM time (earlier VM attempts included,
//! a paused run's resumed one too; never a moved run's laptop minutes, nor time
//! frozen or paused); `--timeout` replaces every lane cap and counts the same.
//! A job judged big becomes max in place, in whichever slot it holds.

use crate::history::Expected;
use crate::outcome::{Lane, LaneDecision, Probe};
use crate::tlc::Counts;

/// The probe's and the caps' settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// A short job still running after this much VM time becomes slow.
    pub short_secs: u64,
    /// The probe judges a slow job this long after it entered the slow lane.
    pub probe_secs: u64,
    /// The probe's window: its first sample is this long before its second.
    pub window_secs: u64,
    /// Big when queue / distinct is at least this, in thousandths.
    pub ratio_permille: u64,
    /// Big when the queue grew by at least this share of distinct, in thousandths.
    pub growth_permille: u64,
    /// The slow lane's cap.
    pub slow_cap_secs: u64,
    /// The max lane's cap.
    pub max_cap_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            short_secs: 15 * 60,
            probe_secs: 15 * 60,
            window_secs: 5 * 60,
            ratio_permille: 400,
            growth_permille: 100,
            slow_cap_secs: 2 * 3600,
            max_cap_secs: 12 * 3600,
        }
    }
}

/// The lane a run is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Current {
    /// Short: slow at `short_secs`.
    Short,
    /// Slow (or unknown), since this much VM time: probed `probe_secs` later.
    Slow(u64),
    /// Probed, or ok and not probed again.
    Probed,
    /// Max: the probe is shown, never acted on.
    Max,
}

/// What the supervisor is to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Keep running.
    Run,
    /// The cap passed: the timed-out marker, then stop TLC.
    Timeout,
}

/// One attempt's lane watch.
#[derive(Debug, Clone)]
pub struct Watch {
    settings: Settings,
    /// The run's lane.
    pub lane: Current,
    timeout_secs: Option<u64>,
    vm_secs_before: u64,
    /// The run's lane decisions, earlier attempts' included.
    pub decisions: Vec<LaneDecision>,
    curve: Vec<(u64, Counts)>,
}

impl Watch {
    /// The watch of an attempt of a run expected `expected`, after
    /// `vm_secs_before` of earlier VM attempts that made `decisions`.
    pub fn new(
        settings: Settings,
        expected: Option<Expected>,
        timeout_secs: Option<u64>,
        vm_secs_before: u64,
        decisions: Vec<LaneDecision>,
    ) -> Self {
        let promoted = decisions
            .iter()
            .find(|d| d.to == Lane::Slow && d.probe.is_none());
        let lane = if decisions.iter().any(|d| d.to == Lane::Max) || expected == Some(Expected::Max)
        {
            Current::Max
        } else if decisions.iter().any(|d| d.probe.is_some()) {
            Current::Probed
        } else if let Some(d) = promoted {
            Current::Slow(d.after_secs)
        } else if expected == Some(Expected::Short) {
            Current::Short
        } else {
            Current::Slow(0)
        };
        Self {
            settings,
            lane,
            timeout_secs,
            vm_secs_before,
            decisions,
            curve: Vec::new(),
        }
    }

    /// A progress line, `secs` into the attempt.
    pub fn progress(&mut self, secs: u64, counts: Counts) {
        self.curve.push((secs, counts));
    }

    /// What to do `secs` into the attempt (its running time, frozen time left
    /// out).
    pub fn tick(&mut self, secs: u64) -> Act {
        let run_secs = self.vm_secs_before + secs;
        if self.lane == Current::Short && run_secs >= self.settings.short_secs {
            self.decide(Lane::Slow, run_secs, None);
            self.lane = Current::Slow(run_secs);
        }
        if let Current::Slow(since) = self.lane {
            if run_secs >= since + self.settings.probe_secs {
                self.probe(secs, run_secs);
            }
        }
        // A max run's probe is shown, never acted on: once, at the probe time.
        let probed = self.decisions.iter().any(|d| d.probe.is_some());
        if self.lane == Current::Max && !probed && run_secs >= self.settings.probe_secs {
            if let Some(probe) = self.measure(secs) {
                self.decide(Lane::Max, run_secs, Some(probe));
            }
        }
        if run_secs >= self.cap_secs() {
            Act::Timeout
        } else {
            Act::Run
        }
    }

    /// The cap, in seconds of the run's VM time: `--timeout`'s, else the lane's.
    pub fn cap_secs(&self) -> u64 {
        self.timeout_secs.unwrap_or(match self.lane {
            Current::Max => self.settings.max_cap_secs,
            _ => self.settings.slow_cap_secs,
        })
    }

    /// Seconds left before the cap, `secs` into the attempt.
    pub fn left(&self, secs: u64) -> u64 {
        self.cap_secs().saturating_sub(self.vm_secs_before + secs)
    }

    /// The probe, `secs` into the attempt: `big` makes the job max in place,
    /// `ok` keeps it slow. Either way its numbers are recorded. Without both
    /// samples yet (a resumed attempt's curve starts empty) it waits for them.
    fn probe(&mut self, secs: u64, run_secs: u64) {
        let Some(probe) = self.measure(secs) else {
            return;
        };
        self.lane = Current::Probed;
        let s = &self.settings;
        let distinct = u128::from(probe.distinct);
        let wide = u128::from(probe.queue) * 1000 >= distinct * u128::from(s.ratio_permille);
        let grew = u128::from(probe.queue.saturating_sub(probe.queue_before)) * 1000
            >= distinct * u128::from(s.growth_permille);
        if wide || grew {
            self.decide(Lane::Max, run_secs, Some(probe));
            self.lane = Current::Max;
        } else {
            self.decide(Lane::Slow, run_secs, Some(probe));
        }
    }

    /// The probe's numbers `secs` into the attempt: now, and a window before.
    fn measure(&self, secs: u64) -> Option<Probe> {
        let at = |t: u64| {
            self.curve
                .iter()
                .rev()
                .find(|(s, _)| *s <= t)
                .map(|(_, c)| *c)
        };
        let (before, now) = (
            at(secs.saturating_sub(self.settings.window_secs))?,
            at(secs)?,
        );
        Some(Probe {
            distinct: now.distinct,
            queue: now.queue,
            queue_before: before.queue,
        })
    }

    /// Record a lane decision.
    fn decide(&mut self, to: Lane, after_secs: u64, probe: Option<Probe>) {
        self.decisions.push(LaneDecision {
            to,
            after_secs,
            probe,
        });
    }
}

#[cfg(test)]
#[path = "lanes_tests.rs"]
mod tests;

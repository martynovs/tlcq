//! Reading TLC (design D8, D11): its progress lines, the counts and error lines
//! of its log, and the verdict its exit status and errors give against the
//! configuration's `must fail` header, as `tla/tlc.sh` judges them.

use serde::{Deserialize, Serialize};

use crate::must_fail::MustFail;
use crate::outcome::Outcome;

/// TLC's counters at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    /// The search depth.
    pub depth: u64,
    /// States generated.
    pub generated: u64,
    /// Distinct states found.
    pub distinct: u64,
    /// States left on the queue.
    pub queue: u64,
}

/// The queue as TLC counts it, every state found and not yet expanded, from
/// progress lines that may be tlx's: its level-synchronous search counts only
/// the states of the level being expanded, so its queue drains to nothing at
/// the end of each level and jumps by the whole next level at the boundary.
/// A queue that grows across a boundary by more than the distinct states
/// found since shows it (TLC's cannot: each new state is queued once); from
/// then on, the states found since the level started are added to it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Pending {
    last: Option<Counts>,
    /// The distinct states when the level being expanded started.
    start: u64,
    /// Whether the queue is known to count one level only.
    levels: bool,
}

impl Pending {
    /// Counting one level only from the start: the lines are known to be
    /// tlx's.
    pub fn levels() -> Self {
        Self {
            levels: true,
            ..Self::default()
        }
    }

    /// Whether the lines so far show a queue of one level only.
    pub fn counts_levels(&self) -> bool {
        self.levels
    }

    /// The next line's counters, its queue as TLC counts it.
    pub fn add(&mut self, c: Counts) -> Counts {
        match self.last {
            Some(p) if c.depth > p.depth => {
                self.levels |= c.queue > p.queue + c.distinct.saturating_sub(p.distinct);
                self.start = p.distinct;
            }
            None => self.start = c.distinct,
            Some(_) => {}
        }
        self.last = Some(c);
        if !self.levels {
            return c;
        }
        Counts {
            queue: c.queue + c.distinct.saturating_sub(self.start),
            ..c
        }
    }
}

/// One `Progress(…)` line's counters, if `line` is one.
pub fn progress(line: &str) -> Option<Counts> {
    let rest = line.strip_prefix("Progress(")?;
    let (depth, rest) = rest.split_once(')')?;
    let (_, rest) = rest.split_once(": ")?;
    let (generated, distinct, queue) = clauses(rest)?;
    Some(Counts {
        depth: number(depth)?,
        generated,
        distinct,
        queue,
    })
}

/// When TLC wrote the progress line `line`, from its `at YYYY-MM-DD HH:MM:SS`,
/// in seconds since 1970 of its local time: only differences between two of
/// them mean anything.
pub fn progress_time(line: &str) -> Option<i64> {
    let (_, rest) = line.strip_prefix("Progress(")?.split_once(") at ")?;
    let stamp = rest.get(..19)?;
    let field = |from: usize, to: usize| stamp.get(from..to)?.parse::<i64>().ok();
    let days = days_from_civil(field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (hour, minute, second) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Whether `line` says TLC has just finished writing a checkpoint.
pub fn checkpointed(line: &str) -> bool {
    line.starts_with("Checkpointing completed")
}

/// Whether `line` says TLC has started writing a checkpoint.
pub fn checkpointing(line: &str) -> bool {
    line.starts_with("Checkpointing of run")
}

/// Whether `line` says the search has ended and the temporal properties are
/// checked over the whole state space: no checkpoint comes after it.
pub fn final_liveness(line: &str) -> bool {
    line.contains("temporal properties for the complete state space")
}

/// The states TLC's recovery from a checkpoint examined, if `line` says it
/// completed: `Recovery completed. N states examined. …`. TLC's count of
/// generated states starts again from there.
pub fn recovered(line: &str) -> Option<u64> {
    let rest = line.strip_prefix("Recovery completed. ")?;
    let (n, rest) = rest.split_once(' ')?;
    rest.starts_with("states examined").then_some(())?;
    n.parse().ok()
}

/// The fingerprint function (`-fp`) TLC's log says it runs with, in the line
/// `Running … with fp N and seed …`.
pub fn fingerprint_fn(log: &str) -> Option<u64> {
    let line = log.lines().find(|l| l.starts_with("Running "))?;
    let (_, rest) = line.split_once(" with fp ")?;
    rest.split(' ').next()?.parse().ok()
}

/// The three counters of `G states generated…, N distinct states found…, Q states
/// left on queue.`, rates in brackets or not.
fn clauses(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.split(", ");
    let mut take = |phrase: &str| {
        let part = parts.next()?;
        let (n, words) = part.split_once(' ')?;
        words.starts_with(phrase).then_some(())?;
        number(n)
    };
    Some((
        take("states generated")?,
        take("distinct states found")?,
        take("states left on queue")?,
    ))
}

/// A count as TLC prints it, with thousands separators.
fn number(s: &str) -> Option<u64> {
    s.replace(',', "").parse().ok()
}

/// What a TLC log says, read once TLC has exited.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// The final counts, else the last progress line's.
    pub counts: Option<Counts>,
    /// Its `Error:` lines.
    pub errors: Vec<String>,
    /// Whether the JVM ran out of memory.
    pub out_of_memory: bool,
    /// Whether tlx wrote the log (its first line names it), not TLC.
    #[serde(default)]
    pub tlx: bool,
}

/// Read a TLC log, or tlx's, which follows TLC's format (tlx-engine E5).
pub fn summary(log: &str) -> Summary {
    let mut s = Summary {
        tlx: log.starts_with("tlx version "),
        ..Summary::default()
    };
    let mut depth = None;
    for line in log.lines() {
        if let Some(c) = progress(line) {
            s.counts = Some(c);
        } else if let Some((generated, distinct, queue)) = clauses(line) {
            s.counts = Some(Counts {
                depth: s.counts.map_or(0, |c| c.depth),
                generated,
                distinct,
                queue,
            });
        } else if let Some(d) = line
            .strip_prefix("The depth of the complete state graph search is ")
            .and_then(|d| number(d.trim_end_matches('.')))
        {
            depth = Some(d);
        } else if line.starts_with("Error: ") {
            s.errors.push(line.to_owned());
        }
        s.out_of_memory |= line.contains("OutOfMemoryError");
    }
    if let (Some(c), Some(d)) = (&mut s.counts, depth) {
        c.depth = d;
    }
    s
}

/// The verdict (D8) on a TLC that exited with `exit` (`None`: killed by a
/// signal, or no exit), judged against the configuration's `must fail`.
pub fn verdict(exit: Option<i32>, expected: Option<&MustFail>, summary: &Summary) -> Outcome {
    let code = match exit {
        Some(code @ (0 | 10..=14)) => code,
        Some(75..=77 | 150..=151) => return Outcome::Unexpected,
        other => {
            return Outcome::Error {
                reason: Some(error_reason(other, summary)),
            };
        }
    };
    let met = match expected {
        None => code == 0,
        Some(MustFail::Deadlock) => {
            code != 0
                && summary
                    .errors
                    .iter()
                    .any(|e| e.starts_with("Error: Deadlock reached"))
        }
        Some(MustFail::Property(p)) => {
            code != 0
                && summary.errors.iter().any(|e| {
                    has_word(e, p) && (e.contains("violated") || e.contains("never witnessed"))
                })
        }
    };
    if met {
        Outcome::Ok
    } else {
        Outcome::Unexpected
    }
}

/// Why a run that exited with `exit` outside the verdict's codes is an
/// `error`: TLC's JVM out of memory; tlx's panic (101), or its own reason
/// for a system error (255: memory for liveness, the disk, a checkpoint it
/// refuses); else the engine's exit.
fn error_reason(exit: Option<i32>, summary: &Summary) -> String {
    let engine = if summary.tlx { "tlx" } else { "TLC" };
    if summary.out_of_memory {
        return "out of memory".to_owned();
    }
    match exit {
        Some(101) if summary.tlx => "tlx panicked".to_owned(),
        Some(255) if summary.tlx => summary.errors.last().map_or_else(
            || "tlx exited 255".to_owned(),
            |e| e.trim_start_matches("Error: ").to_owned(),
        ),
        Some(c) => format!("{engine} exited {c}"),
        None => format!("{engine} did not exit"),
    }
}

/// The hint on a run tlx refused (exit 2: an option; 150: the spec or the
/// configuration; 101: a panic): TLC may run it (tlx-engine E6).
pub fn refusal(exit: Option<i32>, summary: &Summary) -> Option<String> {
    let code = exit.filter(|c| summary.tlx && matches!(c, 2 | 101 | 150))?;
    let first = summary
        .errors
        .first()
        .map_or("no Error: line", |e| e.trim_start_matches("Error: "));
    Some(format!(
        "tlx refused (exit {code}): {first}; resubmit with --engine tlc if TLC accepts it"
    ))
}

/// The word on an `unexpected` verdict of a configuration that must fail:
/// what TLC reported instead, and `--must-fail-only`, which checks only what
/// the header names (D8).
pub fn note(outcome: &Outcome, expected: Option<&MustFail>, summary: &Summary) -> Option<String> {
    if *outcome != Outcome::Unexpected {
        return None;
    }
    let wanted = match expected? {
        MustFail::Deadlock => "a deadlock".to_owned(),
        MustFail::Property(p) => p.clone(),
    };
    let reported = summary.errors.iter().find(|e| {
        e.contains("violated")
            || e.contains("never witnessed")
            || e.starts_with("Error: Deadlock reached")
    })?;
    let reported = reported.trim_start_matches("Error: ");
    let engine = if summary.tlx { "tlx" } else { "TLC" };
    Some(format!(
        "{engine} reported \"{reported}\", not {wanted}; --must-fail-only checks only {wanted}"
    ))
}

/// Whether `word` occurs in `line` as a whole word.
fn has_word(line: &str, word: &str) -> bool {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices(word).any(|(at, _)| {
        let before = line.get(..at).and_then(|s| s.chars().next_back());
        let after = line.get(at + word.len()..).and_then(|s| s.chars().next());
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

#[cfg(test)]
#[path = "tlc_tests.rs"]
mod tests;

//! A run's progress curve (design D15): one row per TLC progress line, in
//! seconds of awake time since the attempt started, with what the estimators
//! need besides: whether the configuration has temporal properties, when
//! TLC's final temporal check started, whether the search was explored to the
//! end, the final counts and the end.
//!
//! On disk it is JSONL, [`FILE`], appended as TLC prints, so a curve cut off
//! by a crash still reads up to its last whole line. Each line is one object,
//! its `kind` saying what it is:
//!
//! ```text
//! {"kind":"temporal"}              the configuration has temporal properties
//! {"kind":"progress","secs":63,"depth":14,"generated":9234007,"distinct":3379510,"queue":1456153}
//! {"kind":"check","secs":1100}     the final temporal check started
//! {"kind":"complete"}              explored to the end, no error
//! {"kind":"final","generated":9811782,"distinct":3000000}
//! {"kind":"end","secs":1234}
//! ```
//!
//! Curves written before were tab-separated ([`LEGACY_FILE`]: a row per
//! progress line, `#` lines for the rest); they still read.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::proto::Counters;
use crate::tlc;

/// An attempt's curve, in its directory.
pub const FILE: &str = "curve.jsonl";

/// An attempt's curve as it was written before JSONL, tab-separated.
pub const LEGACY_FILE: &str = "curve.tsv";

/// One line of a curve's JSONL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Line {
    Temporal,
    Progress {
        secs: u64,
        depth: u64,
        generated: u64,
        distinct: u64,
        queue: u64,
    },
    Check {
        secs: u64,
    },
    Complete,
    Final {
        generated: u64,
        distinct: u64,
    },
    End {
        secs: u64,
    },
}

/// One progress line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// Seconds of awake time since the attempt started.
    pub secs: f64,
    /// The search depth.
    pub depth: u64,
    /// States generated.
    pub generated: f64,
    /// Distinct states found.
    pub distinct: f64,
    /// States left on the queue.
    pub queue: f64,
}

/// A run's progress curve.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Curve {
    /// The progress lines, in order.
    pub points: Vec<Point>,
    /// Whether the configuration has temporal properties.
    pub temporal: bool,
    /// When TLC's final temporal check started, if it has.
    pub check_secs: Option<f64>,
    /// Whether the search was explored to the end with no error.
    pub complete: bool,
    /// The final counts: generated and distinct.
    pub last: Option<(f64, f64)>,
    /// When TLC ended, if it has.
    pub end_secs: Option<f64>,
    /// tlx's count of what was left of the level at each progress line, when
    /// the queue is made TLC's count ([`tlc::Pending`]); else empty.
    pub left: Vec<f64>,
}

impl Curve {
    /// Read a curve from its JSONL, or from the TSV of a curve written before;
    /// a line that does not parse is skipped.
    pub fn parse(text: &str) -> Self {
        if text.starts_with('#') {
            return Self::parse_legacy(text);
        }
        let mut c = Curve::default();
        for line in text.lines().filter_map(|l| serde_json::from_str(l).ok()) {
            match line {
                Line::Temporal => c.temporal = true,
                Line::Progress {
                    secs,
                    depth,
                    generated,
                    distinct,
                    queue,
                } => c.points.push(Point {
                    secs: real(secs),
                    depth,
                    generated: real(generated),
                    distinct: real(distinct),
                    queue: real(queue),
                }),
                Line::Check { secs } => c.check_secs = Some(real(secs)),
                Line::Complete => c.complete = true,
                Line::Final {
                    generated,
                    distinct,
                } => c.last = Some((real(generated), real(distinct))),
                Line::End { secs } => c.end_secs = Some(real(secs)),
            }
        }
        c.left = pending(&mut c.points);
        c
    }

    /// Read a curve from the TSV it was written in before JSONL.
    fn parse_legacy(text: &str) -> Self {
        let mut c = Curve::default();
        for line in text.lines() {
            if let Some(mark) = line.strip_prefix("# ") {
                let mut words = mark.split_whitespace();
                let key = words.next();
                let nums: Vec<f64> = words.filter_map(|w| w.parse().ok()).collect();
                match (key, &nums[..]) {
                    (Some("temporal"), _) => c.temporal = true,
                    (Some("complete"), _) => c.complete = true,
                    (Some("check"), [s]) => c.check_secs = Some(*s),
                    (Some("end"), [s]) => c.end_secs = Some(*s),
                    (Some("final"), [g, n]) => c.last = Some((*g, *n)),
                    _ => {}
                }
                continue;
            }
            let f: Vec<f64> = line.split('\t').filter_map(|v| v.parse().ok()).collect();
            if let [secs, depth, generated, distinct, queue] = f[..] {
                c.points.push(Point {
                    secs,
                    depth: whole(depth),
                    generated,
                    distinct,
                    queue,
                });
            }
        }
        c.left = pending(&mut c.points);
        c
    }

    /// The curve as its JSONL.
    pub fn to_jsonl(&self) -> String {
        let mut lines = Vec::new();
        if self.temporal {
            lines.push(Line::Temporal);
        }
        lines.extend(self.points.iter().map(|p| Line::Progress {
            secs: whole(p.secs),
            depth: p.depth,
            generated: whole(p.generated),
            distinct: whole(p.distinct),
            queue: whole(p.queue),
        }));
        if let Some(s) = self.check_secs {
            lines.push(Line::Check { secs: whole(s) });
        }
        if self.complete {
            lines.push(Line::Complete);
        }
        if let Some((g, n)) = self.last {
            lines.push(Line::Final {
                generated: whole(g),
                distinct: whole(n),
            });
        }
        if let Some(s) = self.end_secs {
            lines.push(Line::End { secs: whole(s) });
        }
        lines.iter().map(jsonl).collect()
    }

    /// The size of each level whose start and the one before it the curve
    /// saw, by depth: the distinct states found while the level before it
    /// was expanded, from the last line of each depth.
    fn level_sizes(&self) -> Vec<(u64, f64)> {
        let starts: Vec<(u64, f64)> = self
            .points
            .iter()
            .zip(self.points.iter().skip(1))
            .filter(|(before, after)| after.depth > before.depth)
            .map(|(before, after)| (after.depth, before.distinct))
            .collect();
        starts
            .iter()
            .zip(starts.iter().skip(1))
            .map(|((_, from), (depth, to))| (*depth, to - from))
            .collect()
    }

    /// The search's depth now and how deep it is likely to go: as deep as
    /// `previous`, a complete run of the configuration; else, once its last
    /// three levels shrink, as many levels on as the current one takes to
    /// fall below one state at the latest shrink ratio.
    pub fn depth(&self, previous: Option<&Curve>) -> Option<Depth> {
        let now = self.points.last()?.depth;
        if let Some(total) = previous.and_then(|p| p.points.last()).map(|p| p.depth) {
            return Some(Depth {
                now,
                total: Some(total),
                from: Some(DepthSource::Previous),
            });
        }
        let sizes = self.level_sizes();
        let shrinking = match sizes.as_slice() {
            [.., (_, a), (_, b), (d, c)] if *d == now && b < a && c < b && *c >= 1.0 => {
                Some((c / b, *c))
            }
            _ => None,
        };
        let total = shrinking.map(|(ratio, size)| {
            let left = (size.ln() / (1.0 / ratio).ln()).ceil();
            now + whole(left)
        });
        Some(Depth {
            now,
            total,
            from: total.map(|_| DepthSource::Shrinking),
        })
    }

    /// The level tlx expands at the last progress line, its rest's change
    /// fitted over the `window` seconds before; `None` for TLC's queue.
    pub fn level(&self, window: f64) -> Option<Level> {
        let (now, left) = self.points.last().zip(self.left.last())?;
        let lines: Vec<(f64, f64)> = self
            .points
            .iter()
            .zip(&self.left)
            .filter(|(p, _)| p.depth == now.depth && p.secs >= now.secs - window)
            .map(|(p, l)| (p.secs / 60.0, *l))
            .collect();
        Some(Level {
            depth: now.depth,
            left: *left,
            per_min: fit(&lines),
        })
    }

    /// The counters at the last progress line.
    pub fn counters(&self) -> Option<Counters> {
        let now = self.points.last()?;
        Some(Counters {
            generated: whole(now.generated),
            distinct: whole(now.distinct),
            queue: whole(now.queue),
        })
    }

    /// TLC's rates over the `window` seconds before the last progress line
    /// (D11): generated and distinct states per minute, and the queue's growth
    /// as a share of the distinct states. `None` before two lines.
    pub fn rates(&self, window: f64) -> Option<Rates> {
        let now = self.points.last()?;
        let then = self
            .points
            .iter()
            .rev()
            .find(|p| p.secs <= now.secs - window)
            .or(self.points.first())?;
        let minutes = (now.secs - then.secs) / 60.0;
        let slope = self.queue_slope(now.secs, window);
        (minutes > 0.0 && now.distinct > 0.0).then(|| Rates {
            generated_per_min: (now.generated - then.generated) / minutes,
            distinct_per_min: (now.distinct - then.distinct) / minutes,
            queue_share: now.queue / now.distinct,
            queue_growth: (now.queue - then.queue) / now.distinct,
            queue_per_min: slope,
        })
    }

    /// The queue's slope, states per minute, fitted over the `window` seconds
    /// up to `at`; `None` with fewer than two lines in it.
    fn queue_slope(&self, at: f64, window: f64) -> Option<f64> {
        let lines: Vec<(f64, f64)> = self
            .points
            .iter()
            .filter(|p| p.secs <= at && p.secs >= at - window)
            .map(|p| (p.secs / 60.0, p.queue))
            .collect();
        fit(&lines)
    }

    /// The queue's slope (as [`Rates::queue_per_min`]) at each progress line
    /// of the last `span` seconds but the first, oldest first: how the slope
    /// itself grows, holds or shrinks. A line less than `window` in is fitted
    /// over the lines so far, as the rate then was.
    pub fn queue_slopes(&self, window: f64, span: f64) -> Vec<f64> {
        let (Some(first), Some(last)) = (self.points.first(), self.points.last()) else {
            return Vec::new();
        };
        self.points
            .iter()
            .filter(|p| p.secs >= last.secs - span && p.secs > first.secs)
            .filter_map(|p| self.queue_slope(p.secs, window))
            .collect()
    }

    /// Read the curve at `path`; `None` if there is none.
    ///
    /// # Errors
    /// If it exists and cannot be read.
    pub fn read(path: &Path) -> std::io::Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(Self::parse(&text))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Read the curve in the attempt directory `dir`: [`FILE`], else the
    /// [`LEGACY_FILE`] of an attempt from before; `None` if neither is there.
    ///
    /// # Errors
    /// If one exists and cannot be read.
    pub fn read_in(dir: &Path) -> std::io::Result<Option<Self>> {
        Self::read(&path_in(dir))
    }
}

/// The curve in the attempt directory `dir`: [`FILE`], unless only an older
/// attempt's [`LEGACY_FILE`] is there.
pub fn path_in(dir: &Path) -> PathBuf {
    let legacy = dir.join(LEGACY_FILE);
    if !dir.join(FILE).exists() && legacy.exists() {
        legacy
    } else {
        dir.join(FILE)
    }
}

/// The text of the curve in the attempt directory `dir`, as
/// [`Curve::read_in`] finds it.
///
/// # Errors
/// If one exists and cannot be read.
pub fn text_in(dir: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path_in(dir)) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The least-squares slope of `(x, y)` points; `None` for fewer than two
/// distinct `x`.
fn fit(points: &[(f64, f64)]) -> Option<f64> {
    #[expect(clippy::cast_precision_loss, reason = "a count of progress lines")]
    let n = points.len() as f64;
    let (sx, sy) = points
        .iter()
        .fold((0.0, 0.0), |(sx, sy), (x, y)| (sx + x, sy + y));
    let (mx, my) = (sx / n, sy / n);
    let (sxy, sxx) = points.iter().fold((0.0, 0.0), |(sxy, sxx), (x, y)| {
        (sxy + (x - mx) * (y - my), sxx + (x - mx) * (x - mx))
    });
    (sxx > 0.0).then(|| sxy / sxx)
}

/// A depth or a count read back as a whole number.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a depth or a count, a whole number far below 2^52"
)]
fn whole(v: f64) -> u64 {
    v.max(0.0) as u64
}

/// A count as the curve keeps it.
#[expect(
    clippy::cast_precision_loss,
    reason = "seconds and state counts are far below 2^52"
)]
fn real(v: u64) -> f64 {
    v as f64
}

/// Make the `points`' queues TLC's count ([`tlc::Pending`]) if they show
/// tlx's count of one level, from the first point on. What is left of a
/// level only shrinks as it is expanded: a line that shows less than a later
/// one of the same level was taken while the level was still being queued
/// (tlx's first line of a level can be), and counts that later one's.
/// Returns what was left of the level at each point, as so counted (none
/// for TLC's queue).
fn pending(points: &mut [Point]) -> Vec<f64> {
    let counts = |p: &Point| tlc::Counts {
        depth: p.depth,
        generated: whole(p.generated),
        distinct: whole(p.distinct),
        queue: whole(p.queue),
    };
    let mut seen = tlc::Pending::default();
    for p in points.iter() {
        seen.add(counts(p));
    }
    if !seen.counts_levels() {
        return Vec::new();
    }
    let mut later: Option<(u64, f64)> = None;
    for p in points.iter_mut().rev() {
        if let Some((_, queue)) = later.filter(|(depth, _)| *depth == p.depth) {
            p.queue = p.queue.max(queue);
        }
        later = Some((p.depth, p.queue));
    }
    let left = points.iter().map(|p| p.queue).collect();
    let mut pending = tlc::Pending::levels();
    for p in points {
        p.queue = real(pending.add(counts(p)).queue);
    }
    left
}

/// How deep a search is and is likely to go ([`Curve::depth`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Depth {
    /// The depth of its last progress line.
    pub now: u64,
    /// The depth it is likely to end at, when known.
    pub total: Option<u64>,
    /// Where that comes from.
    pub from: Option<DepthSource>,
}

/// Where an estimate of a search's final depth comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DepthSource {
    /// The depth a complete run of the configuration ended at.
    Previous,
    /// The run's own levels, shrinking.
    Shrinking,
}

/// The level tlx is expanding: its depth, what is left of it, and how that
/// changes per minute (a fit over the window; `None` with fewer than two
/// lines of the level in it).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Level {
    /// The depth.
    pub depth: u64,
    /// The states of the level not yet expanded.
    pub left: f64,
    /// Their change per minute: negative as the level drains.
    pub per_min: Option<f64>,
}

/// A curve line as JSONL, its newline included.
fn jsonl(line: &Line) -> String {
    serde_json::to_string(line).expect("a curve line serializes") + "\n"
}

/// TLC's rates over a window (D11).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    /// Generated states per minute.
    pub generated_per_min: f64,
    /// Distinct states per minute.
    pub distinct_per_min: f64,
    /// The queue as a share of the distinct states.
    pub queue_share: f64,
    /// The queue's growth over the window, as a share of the distinct states.
    pub queue_growth: f64,
    /// The queue's slope, states per minute: a least-squares fit over the
    /// sliding window, positive while the search widens, negative once it
    /// drains ([`Curve::queue_slopes`] gives its history); `None` with fewer
    /// than two lines in the window.
    pub queue_per_min: Option<f64>,
}

/// A curve written as TLC prints: each log line is offered with the awake
/// seconds it was seen at.
#[derive(Debug)]
pub struct Recorder {
    path: PathBuf,
    checking: bool,
    /// For a recovery, the run's generated states at its checkpoint.
    recovering: Option<u64>,
    /// What TLC's generated count lacks: after a recovery TLC counts again
    /// from the states it recovered, not from those generated before.
    offset: u64,
    /// The generated states at the last progress line, the offset added.
    generated: Option<u64>,
    /// The last progress line's recorded seconds, and TLC's time on it.
    timed: Option<(u64, i64)>,
}

/// How far TLC's spacing of two progress lines may stray from the recorder's
/// and still be taken: past it lies a sleep (awake time, which the curve
/// counts, stops; TLC's clock does not) or a change of the clock.
const SLACK_SECS: u64 = 30;

impl Recorder {
    /// A recorder writing a new curve at `path`.
    ///
    /// # Errors
    /// If the file cannot be written.
    pub fn new(path: PathBuf) -> std::io::Result<Self> {
        std::fs::write(&path, "")?;
        Ok(Self {
            path,
            checking: false,
            recovering: None,
            offset: 0,
            generated: None,
            timed: None,
        })
    }

    /// The seconds to record a progress line written at TLC's time `at` and
    /// seen `seen` seconds in. The recorder sees TLC's lines only when it
    /// looks, some seconds late, so a line goes TLC's own time after the one
    /// before, as long as that keeps within [`SLACK_SECS`] of when it was seen.
    fn stamp(&mut self, seen: u64, at: Option<i64>) -> u64 {
        let by_tlc = match (self.timed, at) {
            (Some((before, then)), Some(now)) => u64::try_from(now - then).ok().map(|d| before + d),
            _ => None,
        };
        let secs = by_tlc
            .filter(|s| s.abs_diff(seen) <= SLACK_SECS)
            .unwrap_or(seen);
        self.timed = at.map(|t| (secs, t));
        secs
    }

    /// TLC recovers a checkpoint taken at `generated` states generated: its
    /// counts from the recovery on are carried on from there.
    pub fn recovering(&mut self, generated: u64) {
        self.recovering = Some(generated);
    }

    /// The generated states at the last progress line, as the run counts
    /// them (across a recovery).
    pub fn generated(&self) -> Option<u64> {
        self.generated
    }

    /// The run's generated states for TLC's count `generated`.
    pub fn carried(&self, generated: u64) -> u64 {
        generated + self.offset
    }

    /// Record what `line` of TLC's log, seen at `secs`, adds to the curve.
    ///
    /// # Errors
    /// If the file cannot be written.
    pub fn line(&mut self, secs: u64, line: &str) -> std::io::Result<()> {
        if let (Some(at), Some(examined)) = (self.recovering, tlc::recovered(line)) {
            self.offset = at.saturating_sub(examined);
        }
        let entry = if let Some(c) = tlc::progress(line) {
            let generated = self.carried(c.generated);
            self.generated = Some(generated);
            Line::Progress {
                secs: self.stamp(secs, tlc::progress_time(line)),
                depth: c.depth,
                generated,
                distinct: c.distinct,
                queue: c.queue,
            }
        } else if line.starts_with("Implied-temporal checking") {
            Line::Temporal
        } else if !self.checking
            && line.starts_with("Checking ")
            && line.contains("temporal properties for the complete state space")
        {
            self.checking = true;
            Line::Check { secs }
        } else if line.starts_with("Model checking completed. No error has been found.") {
            Line::Complete
        } else {
            return Ok(());
        };
        self.append(&jsonl(&entry))
    }

    /// Record the end: TLC's final counts, and when it ended.
    ///
    /// # Errors
    /// If the file cannot be written.
    pub fn end(&mut self, secs: u64, counts: Option<tlc::Counts>) -> std::io::Result<()> {
        let last = counts.map(|c| {
            jsonl(&Line::Final {
                generated: self.carried(c.generated),
                distinct: c.distinct,
            })
        });
        self.append(&(last.unwrap_or_default() + &jsonl(&Line::End { secs })))
    }

    fn append(&self, text: &str) -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().append(true).open(&self.path)?;
        f.write_all(text.as_bytes())
    }
}

#[cfg(test)]
#[path = "curve_tests.rs"]
mod tests;

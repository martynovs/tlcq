//! The ETA (design D15), as the study `tmp/eta/REPORT.md` chose it.
//!
//! Queue / distinct (Q/N) falls along a nearly universal curve as a run works
//! through its share of the states, and generated states grow about linearly in
//! time. So Q/N tells how far along a run is, and the recent generated-states
//! rate turns what is left into time:
//! - with a previous run of the configuration, the geometric mean of H3 (locate
//!   the run on the previous one by Q/N, scale its remaining time by size and
//!   speed) and H4 (fit the previous run's Q/N shape to this one);
//! - on a first run, the geometric mean of N4 (Q/N extrapolated to 0 over the
//!   generated states) and P2 (the population's median Q/N shape fitted with a
//!   free level);
//! - a configuration with temporal properties gets its final temporal check,
//!   which prints no progress, in proportion; once that check starts, its own
//!   tail replaces the estimate.
//!
//! Every time is in seconds since the attempt started. The code follows the
//! study's `est.py` closely, names included, so the two can be read side by
//! side; `eta_tests` checks it gives the study's numbers.

#![expect(
    clippy::indexing_slicing,
    reason = "a curve's arrays share one length and always hold the origin, so each index is in range by construction"
)]
#![expect(
    clippy::many_single_char_names,
    reason = "the study's names: t, g, n, q, x for time, generated, distinct, queue and Q/N"
)]

use crate::curve::Curve;

/// The window of the current rates, in seconds.
const WINDOW: f64 = 300.0;

/// No estimate before this many progress lines…
const MIN_LINES: usize = 3;

/// …and this many seconds into the attempt.
const MIN_SECS: f64 = 300.0;

/// A curve as the estimators read it: the origin prepended, and Q/N.
#[derive(Debug, Clone)]
struct Run {
    t: Vec<f64>,
    g: Vec<f64>,
    n: Vec<f64>,
    x: Vec<f64>,
    /// The end, for a previous run.
    end: f64,
    /// The final temporal check's start, if it started.
    check: Option<f64>,
    /// Whether the configuration has temporal properties.
    temporal: bool,
    /// The final generated and distinct states.
    g_inf: f64,
    n_inf: f64,
}

impl Run {
    fn new(c: &Curve) -> Self {
        let mut t = vec![0.0];
        let (mut g, mut n, mut x) = (vec![0.0], vec![0.0], vec![1.0]);
        for p in &c.points {
            t.push(p.secs);
            g.push(p.generated);
            n.push(p.distinct);
            x.push(if p.distinct > 0.0 {
                p.queue / p.distinct
            } else {
                1.0
            });
        }
        let (g_last, n_last) = (
            *g.last().expect("the origin"),
            *n.last().expect("the origin"),
        );
        let (g_inf, n_inf) = c.last.unwrap_or((g_last, n_last));
        Self {
            end: c.end_secs.unwrap_or(*t.last().expect("the origin")),
            t,
            g,
            n,
            x,
            check: c.check_secs,
            temporal: c.temporal,
            g_inf,
            n_inf,
        }
    }

    /// The progress lines so far.
    fn lines(&self) -> usize {
        self.t.len() - 1
    }

    fn now(&self) -> f64 {
        self.t[self.lines()]
    }

    /// The final temporal check's length, for a previous run.
    fn tail(&self) -> f64 {
        self.check.map_or(0.0, |s| self.end - s)
    }

    /// `arr` at time `tau`.
    fn at(&self, arr: &[f64], tau: f64) -> f64 {
        interp(tau, &self.t, arr)
    }

    /// `arr`'s rate over the window before `tau`.
    fn rate_at(&self, arr: &[f64], tau: f64) -> f64 {
        let a = (tau - WINDOW).max(0.0);
        (self.at(arr, tau) - self.at(arr, a)) / (tau - a).max(1e-9)
    }

    /// `arr`'s rate over the window before the last line.
    fn rate(&self, arr: &[f64]) -> f64 {
        let i = self.lines();
        let after = self.t.partition_point(|v| *v <= self.t[i] - WINDOW);
        let j = after.saturating_sub(1).min(i.saturating_sub(1));
        if self.t[i] <= self.t[j] {
            return 0.0;
        }
        (arr[i] - arr[j]) / (self.t[i] - self.t[j])
    }
}

/// `fp` at `x`, linear between the points `xp` (increasing), held flat
/// outside them.
fn interp(x: f64, xp: &[f64], fp: &[f64]) -> f64 {
    let last = xp.len() - 1;
    if x <= xp[0] {
        return fp[0];
    }
    if x >= xp[last] {
        return fp[last];
    }
    let i = xp.partition_point(|v| *v <= x);
    let (x0, x1, f0, f1) = (xp[i - 1], xp[i], fp[i - 1], fp[i]);
    if x1 <= x0 {
        return f1;
    }
    f0 + (x - x0) * (f1 - f0) / (x1 - x0)
}

/// The least-squares line through the points: slope and intercept.
fn line_fit(xs: &[f64], ys: &[f64]) -> (f64, f64) {
    #[expect(clippy::cast_precision_loss, reason = "a few hundred points")]
    let n = xs.len() as f64;
    let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
    let sxy: f64 = xs.iter().zip(ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    let sxx: f64 = xs.iter().map(|x| (x - mx) * (x - mx)).sum();
    let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    (slope, my - slope * mx)
}

/// The median.
fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    if v.len().is_multiple_of(2) {
        f64::midpoint(v[m - 1], v[m])
    } else {
        v[m]
    }
}

/// `count` points from `a` to `b`, evenly spaced.
fn linspace(a: f64, b: f64, count: usize) -> Vec<f64> {
    #[expect(clippy::cast_precision_loss, reason = "a few hundred points")]
    let step = (b - a) / (count - 1) as f64;
    #[expect(clippy::cast_precision_loss, reason = "a few hundred points")]
    (0..count).map(|i| a + step * i as f64).collect()
}

// ---------- first run ----------

/// The last half of the lines (at least 5), from line 1 on.
fn fit_window(o: &Run) -> std::ops::RangeInclusive<usize> {
    let n = o.lines();
    let m = 5.max(n.div_ceil(2));
    (n + 1).saturating_sub(m).max(1)..=n
}

/// N4: Q/N regressed on generated states over the last half of the lines and
/// extrapolated to 0; the remaining work at the current rate.
fn n4(o: &Run) -> Option<f64> {
    if o.lines() < 3 {
        return None;
    }
    let s = fit_window(o);
    let (b, a) = line_fit(&o.g[s.clone()], &o.x[s]);
    let rate = o.rate(&o.g);
    if b >= 0.0 || rate <= 0.0 {
        return None;
    }
    Some(o.now() + (-a / b - o.g[o.lines()]).max(0.0) / rate)
}

/// The work fractions the shapes are sampled at.
fn ugrid() -> Vec<f64> {
    linspace(0.002, 0.998, 499)
}

/// A run's log(Q/N) over the work fraction (generated / final generated),
/// less its value at half the work (its level), and that level.
fn shape(r: &Run) -> (Vec<f64>, f64) {
    let grid = ugrid();
    let u: Vec<f64> = r.g.iter().map(|g| g / r.g_inf).collect();
    let lx: Vec<f64> = grid
        .iter()
        .map(|v| interp(*v, &u, &r.x).max(1e-4).ln())
        .collect();
    let level = interp(0.5, &grid, &lx);
    (lx.iter().map(|v| v - level).collect(), level)
}

/// Fit log(Q/N) = level + `shape`(generated / final generated) to the last
/// half of the run's lines (and always its last 3); `prior` (mean and spread
/// of the level) holds the level near it when the curve says little. Returns
/// the final generated states.
fn fit_final(o: &Run, shape: &[f64], prior: Option<(f64, f64)>) -> Option<f64> {
    let n = o.lines();
    if n < 3 {
        return None;
    }
    let grid = ugrid();
    let g_now = o.g[n];
    let kept: Vec<(f64, f64)> = (1..=n)
        .filter(|i| o.g[*i] >= 0.5 * g_now || *i + 3 > n)
        .map(|i| (o.g[i], o.x[i].max(1e-4).ln()))
        .collect();
    #[expect(clippy::cast_precision_loss, reason = "a few hundred points")]
    let m = kept.len() as f64;
    let (c, mu) = prior.map_or((0.0, 0.0), |(mu, sd)| ((0.05 / sd).powi(2), mu));
    let (lo, hi) = (0.005_f64.ln(), 0.99_f64.ln());
    let mut best: Option<(f64, f64)> = None;
    for u_now in linspace(lo, hi, 400).into_iter().map(f64::exp) {
        let r: Vec<f64> = kept
            .iter()
            .map(|(g, lx)| lx - interp(u_now / g_now * g, &grid, shape))
            .collect();
        let level = (r.iter().sum::<f64>() + c * mu) / (m + c);
        let sse = r.iter().map(|v| (v - level).powi(2)).sum::<f64>() + c * (level - mu).powi(2);
        if best.is_none_or(|(b, _)| sse < b) {
            best = Some((sse, g_now / u_now));
        }
    }
    best.map(|(_, g_inf)| g_inf)
}

/// The end once `g_inf` states are generated, at the current rate.
fn end_at(o: &Run, g_inf: f64) -> Option<f64> {
    let rate = o.rate(&o.g);
    (rate > 0.0).then(|| o.now() + (g_inf - o.g[o.lines()]).max(0.0) / rate)
}

// ---------- a previous run ----------

/// The previous run's time at the last point where its Q/N was still at or
/// above `x`.
fn match_x(p: &Run, x: f64) -> Option<f64> {
    let i = (1..p.t.len()).rev().find(|i| p.x[*i] >= x)?;
    if i + 1 >= p.t.len() {
        return Some(p.t[p.t.len() - 1]);
    }
    let (x0, x1) = (p.x[i], p.x[i + 1]);
    let f = if (x0 - x1).abs() > 0.0 {
        ((x0 - x) / (x0 - x1)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some(p.t[i] + f * (p.t[i + 1] - p.t[i]))
}

/// H3: locate this run on the previous one by Q/N; its remaining time there,
/// times the size factor, over the speed factor.
fn h3(o: &Run, p: &Run) -> Option<f64> {
    if o.lines() < 2 {
        return None;
    }
    let tau = match_x(p, o.x[o.lines()]).filter(|t| *t > 0.0)?;
    let size = o.n[o.lines()] / p.at(&p.n, tau).max(1.0);
    let (rate, then) = (o.rate(&o.g), p.rate_at(&p.g, tau));
    if rate <= 0.0 || then <= 0.0 {
        return None;
    }
    Some(o.now() + size * (p.end - tau).max(0.0) * then / rate)
}

/// H4: the previous run's own Q/N shape fitted to this run, its level held
/// near the previous one's; its temporal check in proportion.
fn h4(o: &Run, p: &Run, tail_ratio: f64) -> Option<f64> {
    let (logf, level) = shape(p);
    let end = end_at(o, fit_final(o, &logf, Some((level, 0.15)))?)?;
    let ratio = match p.check {
        Some(start) if start > 0.0 && p.tail() > 0.0 => p.tail() / start,
        _ if o.temporal => tail_ratio,
        _ => 0.0,
    };
    Some(end * (1.0 + ratio))
}

/// The geometric mean of two estimates' remaining times; either alone when
/// the other gives none.
fn log_mean(o: &Run, a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let now = o.now();
            Some(now + ((a - now).max(1.0) * (b - now).max(1.0)).sqrt())
        }
        (a, b) => a.or(b),
    }
}

/// The population's median Q/N shape and the median share of the temporal
/// check in a run's time: from other specs' runs, explored to the end.
#[derive(Debug, Clone, Default)]
pub struct Population {
    shape: Option<Vec<f64>>,
    tail_ratio: f64,
}

impl Population {
    /// The population of `curves`; only those explored to the end, with 5
    /// progress lines or more, count.
    pub fn new<'a>(curves: impl IntoIterator<Item = &'a Curve>) -> Self {
        let runs: Vec<Run> = curves
            .into_iter()
            .filter(|c| c.complete && c.points.len() >= 5)
            .map(Run::new)
            .collect();
        let shapes: Vec<Vec<f64>> = runs.iter().map(|r| shape(r).0).collect();
        let shape = (!shapes.is_empty()).then(|| {
            (0..shapes[0].len())
                .map(|i| median(shapes.iter().map(|s| s[i]).collect()))
                .collect()
        });
        let ratios: Vec<f64> = runs
            .iter()
            .filter_map(|r| r.check.filter(|s| *s > 0.0).map(|s| r.tail() / s))
            .collect();
        Self {
            shape,
            tail_ratio: if ratios.is_empty() {
                0.0
            } else {
                median(ratios)
            },
        }
    }
}

/// Where an estimate comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A previous run of the same configuration (the exact or configuration key).
    Previous,
    /// A previous run of the configuration's name, the model since edited: the
    /// mean of that and the first-run estimate.
    Edited,
    /// This run's own curve, and the population's.
    FirstRun,
}

/// An estimated end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Eta {
    /// The end, in seconds since the attempt started.
    pub end_secs: f64,
    /// Its source.
    pub source: Source,
}

/// A previous run to estimate from, and how it was found.
#[derive(Debug, Clone, Copy)]
pub struct Previous<'a> {
    /// Its curve (its last attempt's).
    pub curve: &'a Curve,
    /// Found only by the configuration's name: the model was edited.
    pub by_name: bool,
}

/// The ETA of the attempt whose curve so far is `current`. None before 3
/// progress lines and 5 minutes, or when no estimator answers.
pub fn estimate(current: &Curve, previous: Option<Previous<'_>>, pop: &Population) -> Option<Eta> {
    let o = Run::new(current);
    if o.lines() < MIN_LINES || o.now() < MIN_SECS {
        return None;
    }
    let first = first_run(&o, pop);
    let Some(prev) = previous.filter(|p| p.curve.complete) else {
        return first.map(|end_secs| Eta {
            end_secs,
            source: Source::FirstRun,
        });
    };
    let p = Run::new(prev.curve);
    let history = with_previous(&o, &p, pop.tail_ratio);
    let (end, source) = match (history, first) {
        (Some(h), Some(f)) if prev.by_name => (f64::midpoint(h, f), Source::Edited),
        (Some(h), _) => (h, Source::Previous),
        (None, f) => (f?, Source::FirstRun),
    };
    Some(Eta {
        end_secs: end,
        source,
    })
}

/// The first-run estimate: N4 and P2, then the temporal check.
fn first_run(o: &Run, pop: &Population) -> Option<f64> {
    if let Some(start) = o.check {
        return Some(start * (1.0 + pop.tail_ratio));
    }
    let p2 = pop
        .shape
        .as_ref()
        .and_then(|s| fit_final(o, s, None))
        .and_then(|g| end_at(o, g));
    let end = log_mean(o, n4(o), p2)?;
    Some(if o.temporal {
        end * (1.0 + pop.tail_ratio)
    } else {
        end
    })
}

/// The estimate from a previous run: H3 and H4; once the final temporal check
/// started, the previous run's check scaled to this run's size and speed.
fn with_previous(o: &Run, p: &Run, tail_ratio: f64) -> Option<f64> {
    if let Some(start) = o.check {
        if p.tail() > 0.0 {
            let (rate, then) = (o.rate(&o.g), p.rate_at(&p.g, p.now()));
            let speed = if rate > 0.0 && then > 0.0 {
                then / rate
            } else {
                1.0
            };
            return Some(start + p.tail() * o.n[o.lines()] / p.n_inf.max(1.0) * speed);
        }
        return Some(start * (1.0 + tail_ratio));
    }
    log_mean(o, h3(o, p), h4(o, p, tail_ratio))
}

#[cfg(test)]
#[path = "eta_tests.rs"]
mod tests;

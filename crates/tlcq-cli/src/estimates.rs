//! The ETA's basis on the laptop (design D15): a running job's curve so far,
//! the previous run of its configuration and the population of other specs'
//! runs, all from the history and the curves the laptop keeps.

use crate::curve::Curve;
use crate::eta::{estimate, Eta, Population, Previous};
use crate::history::{History, Keys};
use crate::laptop_runs::Phase;
use crate::proto::{Held, VmState};
use crate::status::{Line, LineEta, View};
use crate::store::{JobRecord, Store};

/// The ETA of a job with the history keys `keys`, its attempt's curve so far
/// `current`: from the latest run explored to the end under the exact or
/// configuration key, else under the name key, else a first-run estimate.
///
/// # Errors
/// If a curve cannot be read.
pub fn eta(
    store: &Store,
    history: &History,
    keys: &[Keys],
    current: &Curve,
) -> std::io::Result<Option<Eta>> {
    let previous = previous(store, history, keys)?;
    let specs: Vec<&str> = keys.iter().map(|k| k.spec.as_str()).collect();
    let engine = keys.first().map(|k| k.engine);
    let mut others = Vec::new();
    for r in history.records() {
        // Other specs' runs on the same engine only (tlx-engine E7).
        if r.names.iter().any(|k| specs.contains(&k.spec.as_str()))
            || r.names.first().map(|k| k.engine) != engine
        {
            continue;
        }
        if let Some(name) = r.curves.last() {
            others.extend(Curve::read(&store.curve_file(name))?);
        }
    }
    let previous = previous.as_ref().map(|(curve, by_name)| Previous {
        curve,
        by_name: *by_name,
    });
    Ok(estimate(current, previous, &Population::new(&others)))
}

/// The last curve of the latest run explored to the end under the exact or
/// configuration key, else under the name key (`true`).
fn previous(
    store: &Store,
    history: &History,
    keys: &[Keys],
) -> std::io::Result<Option<(Curve, bool)>> {
    for by_name in [false, true] {
        let same = |k: &Keys| {
            keys.iter().any(|j| {
                if by_name {
                    k.same_name(j)
                } else {
                    k.same_configuration(j)
                }
            })
        };
        for r in history.records().iter().rev() {
            let Some(name) = r.curves.last().filter(|_| r.names.iter().any(same)) else {
                continue;
            };
            if let Some(c) = Curve::read(&store.curve_file(name))?.filter(|c| c.complete) {
                return Ok(Some((c, by_name)));
            }
        }
    }
    Ok(None)
}

/// Give each running line its estimated end.
///
/// # Errors
/// If the history or a curve cannot be read.
pub(crate) fn with_etas(
    store: &Store,
    records: &std::collections::BTreeMap<String, JobRecord>,
    lines: &mut [Line],
) -> Result<(), crate::snapshot::Error> {
    let mut history = None;
    for l in lines {
        let current = match (&l.view, l.run.as_deref()) {
            // Only while its engine runs: not frozen, paused or waiting for a
            // slot.
            (View::Vm(job), _)
                if job.state == VmState::Running && job.held.as_ref().is_none_or(Held::runs) =>
            {
                job.curve.as_ref().map(|c| Curve::parse(&c.text))
            }
            (View::Laptop(p), Some(run)) if p.phase == Phase::Running => {
                Curve::read_in(&store.run_dir(&l.id, run))?
            }
            _ => None,
        };
        let Some(current) = current else {
            continue;
        };
        let history = match &mut history {
            Some(h) => h,
            None => history.insert(History::load(&store.history())?),
        };
        let keys = records
            .get(&l.id)
            .and_then(|r| r.info.as_ref())
            .map(|i| i.names.clone())
            .unwrap_or_default();
        let last = current.points.last().map_or(0.0, |p| p.secs);
        l.eta = eta(store, history, &keys, &current)?.map(|e| LineEta {
            remaining_secs: whole_secs(e.end_secs - last),
            source: e.source,
        });
    }
    Ok(())
}

/// Seconds as a whole number, none below 0.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "an ETA is a few days at most"
)]
fn whole_secs(secs: f64) -> u64 {
    secs.max(0.0).round() as u64
}

#[cfg(test)]
#[path = "estimates_tests.rs"]
mod tests;

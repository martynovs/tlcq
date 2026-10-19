//! The laptop's runs (design D7): each job's detached supervisor, launched
//! holding the job lock and a slot ticket; the progress it writes; `cancel`;
//! and the orphan rule, which ends a laptop job whose lock is free.

use std::fs::File;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::files::{read_json, write_file, write_json};
use crate::outcome::{JobResult, Outcome};
use crate::proto::Counters;
use crate::runner::store::CancelMarker;
use crate::snapshot::{self, Error};
use crate::store::{JobRecord, PlacementLock, Store};
use crate::submit::Supervisors;

/// A laptop run as its supervisor last wrote it (`progress.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// Where it is.
    pub phase: Phase,
    /// The awake time TLC has run, in seconds: the lower bound of a run that
    /// ends early (D5).
    pub elapsed_secs: u64,
    /// TLC's latest counters, once it has printed any.
    pub counters: Option<Counters>,
    /// When TLC started, in seconds since the epoch.
    #[serde(default)]
    pub started_at: Option<u64>,
    /// Seconds left before the cap, and the cap, if the run has one.
    #[serde(default)]
    pub left_secs: Option<(u64, u64)>,
    /// Generated states per minute, over the last 5 minutes.
    #[serde(default)]
    pub generated_per_min: Option<u64>,
    /// The queue's growth over the last 5 minutes, in thousandths of the
    /// distinct states.
    #[serde(default)]
    pub queue_growth_permille: Option<i64>,
}

/// Where a laptop run is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Phase {
    /// Waiting for the slot, behind this many jobs.
    Slot {
        /// The jobs ahead of it.
        ahead: usize,
    },
    /// Waiting for free disk.
    Disk {
        /// The free disk, in GB.
        free_gb: u64,
    },
    /// TLC runs.
    Running,
}

impl Default for Phase {
    fn default() -> Self {
        Phase::Slot { ahead: 0 }
    }
}

impl Progress {
    /// The progress of run `run` of `id`: waiting for the slot until its
    /// supervisor writes one.
    ///
    /// # Errors
    /// If the file cannot be read.
    pub fn read(store: &Store, id: &str, run: &str) -> std::io::Result<Self> {
        let path = store.run_dir(id, run).join("progress.json");
        Ok(read_json(&path)?.unwrap_or_default())
    }
}

/// Starting and stopping supervisors: a seam, so the rest is tested without
/// processes.
pub trait Processes {
    /// Start the supervisor of run `run` of `id`, whose directory is `dir`, in
    /// its own process group, handing it `lock`. Returns its process id.
    ///
    /// # Errors
    /// If it does not start.
    fn spawn(&mut self, dir: &Path, id: &str, run: &str, lock: File) -> std::io::Result<u32>;

    /// Kill the process group `pid` leads (SIGKILL), TLC with it.
    ///
    /// # Errors
    /// If the signal cannot be sent.
    fn kill(&mut self, pid: u32) -> std::io::Result<()>;
}

/// The laptop's runs, their supervisors started by `processes`.
#[derive(Debug)]
pub struct LaptopRuns<P> {
    /// How supervisors are started and stopped.
    pub processes: P,
}

impl<P: Processes> Supervisors for LaptopRuns<P> {
    fn launch(
        &mut self,
        store: &Store,
        lock: &PlacementLock,
        record: &mut JobRecord,
        tarball: &[u8],
    ) -> std::io::Result<()> {
        let run = record.run.clone().expect("a laptop run is named");
        // Waits only for a supervisor that ended this id's earlier run and is
        // exiting: it has nothing left to do under the placement lock.
        let job_lock = store.job_lock(&record.id)?;
        store.write_ticket(lock, &record.id)?;
        store.write_record(lock, record)?;
        write_file(&store.tarball(&record.id), tarball)?;
        let dir = store.run_dir(&record.id, &run);
        std::fs::create_dir_all(&dir)?;
        let pid = self.processes.spawn(&dir, &record.id, &run, job_lock)?;
        write_file(&dir.join("pid"), pid.to_string().as_bytes())
    }

    fn cancel(
        &mut self,
        store: &Store,
        lock: &PlacementLock,
        record: &mut JobRecord,
        marker: &CancelMarker,
    ) -> std::io::Result<()> {
        let dir = store.run_dir(&record.id, record.run.as_deref().unwrap_or_default());
        std::fs::create_dir_all(&dir)?;
        write_json(&dir.join("cancel.json"), marker)?;
        if store.job_lock_held(&record.id)? {
            let pid = std::fs::read_to_string(dir.join("pid"))?;
            let pid = pid.trim().parse().map_err(std::io::Error::other)?;
            self.processes.kill(pid)?;
            // Taken once the supervisor is gone, and let go at once.
            drop(store.job_lock(&record.id)?);
        }
        orphan(store, lock, record).map_err(|e| match e {
            Error::Io(e) => e,
            e => std::io::Error::other(e),
        })
    }
}

/// The orphan rule (D7), for a laptop job whose lock is free: it ends
/// `cancelled` if a cancel marker is in its run's directory, else with the
/// result its supervisor wrote, else `timeout` if a timed-out marker is there,
/// else `error`; its lower bound goes to the history, its extracted job,
/// scratch and ticket are deleted.
///
/// # Errors
/// If the run's files cannot be read, or the state written.
pub fn orphan(store: &Store, lock: &PlacementLock, record: &mut JobRecord) -> Result<(), Error> {
    let run = record.run.clone().unwrap_or_default();
    let dir = store.run_dir(&record.id, &run);
    let elapsed = Progress::read(store, &record.id, &run)?.elapsed_secs;
    let marker: Option<CancelMarker> = read_json(&dir.join("cancel.json"))?;
    let written: Option<JobResult> = read_json(&dir.join("result.json"))?;
    let result = match (marker, written) {
        (Some(m), _) => laptop_result(
            Outcome::Cancelled {
                by: m.by,
                replaced_by: m.replaced_by,
            },
            elapsed,
        ),
        (None, Some(written)) => written,
        (None, None) if dir.join("timed-out").exists() => laptop_result(Outcome::Timeout, elapsed),
        (None, None) => laptop_result(
            Outcome::Error {
                reason: Some("its supervisor died (the laptop restarted?)".to_owned()),
            },
            elapsed,
        ),
    };
    keep_curve(store, &record.id, &run)?;
    clean(store, &record.id, &run)?;
    if dir.exists() {
        write_json(&dir.join("result.json"), &result)?;
    }
    record.elapsed_secs = result.duration_secs;
    snapshot::end(store, lock, record, &result)
}

/// Keep a laptop run's progress curve, as its attempt 0, for the history.
///
/// # Errors
/// If the curve cannot be copied.
pub fn keep_curve(store: &Store, id: &str, run: &str) -> std::io::Result<()> {
    match std::fs::copy(
        crate::curve::path_in(&store.run_dir(id, run)),
        store.curve_path(id, run, 0),
    ) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Delete a laptop run's extracted job and scratch, and its job's ticket: TLC
/// no longer runs.
///
/// # Errors
/// If a file cannot be deleted.
pub fn clean(store: &Store, id: &str, run: &str) -> std::io::Result<()> {
    let dir = store.run_dir(id, run);
    for sub in ["job", "scratch"] {
        match std::fs::remove_dir_all(dir.join(sub)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    store.remove_ticket(id)
}

/// A laptop run's result that is only an outcome, after `secs` of awake time.
pub fn laptop_result(outcome: Outcome, secs: u64) -> JobResult {
    JobResult {
        outcome,
        exit_status: None,
        distinct: None,
        generated: None,
        depth: None,
        place: "laptop".to_owned(),
        duration_secs: secs,
        lanes: Vec::new(),
        errors: Vec::new(),
        note: None,
    }
}

#[cfg(test)]
#[path = "laptop_runs_tests.rs"]
mod tests;

//! The runner's state directory (design D9, D10, D13): the admission lock, the
//! state document, each job's tarball, and each attempt's directory.
//!
//! ```text
//! <state>/admission.lock     every verb holds it to apply
//! <state>/state.json         the runner's state, rewritten whole under the lock
//! <state>/jobs/<id>.tar      each job's tarball
//! <state>/attempts/<id>/<run>/<n>/
//!     started                the supervisor's first act
//!     cancel.json            a cancel marker: the canceller and any replacing id
//!     pause                  the page's pause request
//!     paused                 the supervisor's stop for a pause
//!     no-checkpoint          its stop of a frozen job with no checkpoint
//!     frozen.log             the page's and admission's freezes, thaws and unpauses
//!     timed-out              the supervisor's stop at its cap
//!     searched               its search has ended: a pause asked now never lands
//!     result.json            the run so far, and the verdict if TLC exited on its own
//!     scratch/               TLC's working directory
//! <state>/checkpoints/<id>/<run>/
//!     checkpoint.json        what recovering it needs
//!     tlc/                   the engine's checkpoint, the run's resume recovers
//!     tlx                    the tlx binary that wrote it, the only one that recovers it
//! <state>/p-closed           the P slot is closed
//! ```

use std::fs::File;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::settle::Observed;
use super::state::{Phase, Run, State};
use crate::files::{read_json, write_file, write_json};
use crate::outcome::{JobResult, LaneDecision};

/// An attempt's `result.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptResult {
    /// The verdict, written only when TLC exits on its own.
    pub finished: Option<JobResult>,
    /// The run's duration so far, in seconds.
    pub duration_secs: u64,
    /// The run's VM running time so far, in seconds, for `--timeout`.
    pub vm_secs: u64,
    /// The run's lane decisions so far.
    pub lanes: Vec<LaneDecision>,
    /// Seconds left before the attempt's cap, as its supervisor last wrote.
    #[serde(default)]
    pub left_secs: Option<u64>,
    /// The cap, in seconds.
    #[serde(default)]
    pub cap_secs: Option<u64>,
    /// The attempt's number.
    #[serde(default)]
    pub attempt: u32,
    /// When its TLC started, in seconds since the epoch.
    #[serde(default)]
    pub started_at: Option<u64>,
    /// When its TLC ended, in seconds since the epoch.
    #[serde(default)]
    pub ended_at: Option<u64>,
    /// The signal that killed the engine, if one did (no exit status then).
    #[serde(default)]
    pub signal: Option<i32>,
    /// Whether TLC ran out of memory.
    #[serde(default)]
    pub out_of_memory: bool,
    /// TLC's log, beside this file.
    #[serde(default)]
    pub log: Option<String>,
    /// The run's end, as settling recorded it: the outcome whichever way the
    /// attempt ended (D8).
    #[serde(default)]
    pub ended: Option<JobResult>,
}

/// A cancel marker's contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelMarker {
    /// The canceller's label.
    pub by: String,
    /// The id that replaced the run, for a supersede.
    pub replaced_by: Option<String>,
}

/// The admission lock, held while the guard lives.
#[derive(Debug)]
pub struct AdmissionLock {
    _file: File,
}

/// The runner's state directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Open the state directory at `root`, creating what is missing.
    pub fn open(root: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(root.join("jobs"))?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// Whether the runner is stopping (D10): it got SIGTERM and has not
    /// started again, so admission, in every process, starts nothing.
    pub fn stopping(&self) -> bool {
        self.root.join("stopping").exists()
    }

    /// Mark the runner stopping, or, at its start, no longer.
    ///
    /// # Errors
    /// If the marker cannot be written or removed.
    pub fn set_stopping(&self, on: bool) -> std::io::Result<()> {
        let marker = self.root.join("stopping");
        if on {
            return write_file(&marker, b"");
        }
        match std::fs::remove_file(&marker) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Take the admission lock, waiting for it.
    pub fn lock(&self) -> std::io::Result<AdmissionLock> {
        let file = File::create(self.root.join("admission.lock"))?;
        file.lock()?;
        Ok(AdmissionLock { _file: file })
    }

    /// Read the state (empty if there is none yet).
    pub fn load(&self, _lock: &AdmissionLock) -> std::io::Result<State> {
        Ok(read_json(&self.root.join("state.json"))?.unwrap_or_default())
    }

    /// Write the state whole.
    pub fn save(&self, _lock: &AdmissionLock, state: &State) -> std::io::Result<()> {
        write_json(&self.root.join("state.json"), state)
    }

    /// Keep a job's tarball, once: an id's bytes never change.
    pub fn put_tarball(
        &self,
        _lock: &AdmissionLock,
        id: &str,
        tarball: &[u8],
    ) -> std::io::Result<()> {
        let path = self.tarball(id);
        if path.exists() {
            return Ok(());
        }
        let tmp = path.with_extension("tmp");
        write_file(&tmp, tarball)?;
        std::fs::rename(&tmp, &path)
    }

    /// Where a job's tarball is kept.
    pub fn tarball(&self, id: &str) -> PathBuf {
        self.root.join("jobs").join(format!("{id}.tar"))
    }

    /// The bytes free to an unprivileged writer on the state volume.
    pub fn free_bytes(&self) -> std::io::Result<u64> {
        let fs = rustix::fs::statvfs(&self.root)?;
        Ok(fs.f_bavail.saturating_mul(fs.f_frsize))
    }

    /// An attempt's directory.
    pub fn attempt_dir(&self, id: &str, run: &str, number: u32) -> PathBuf {
        self.root
            .join("attempts")
            .join(id)
            .join(run)
            .join(number.to_string())
    }

    /// Create an attempt's directory and its first `result.json`.
    pub fn create_attempt(
        &self,
        id: &str,
        run: &str,
        number: u32,
        first: &AttemptResult,
    ) -> std::io::Result<()> {
        let dir = self.attempt_dir(id, run, number);
        std::fs::create_dir_all(dir.join("scratch"))?;
        write_json(&dir.join("result.json"), first)
    }

    /// Record the run's end in its last attempt's `result.json`.
    pub fn record_end(
        &self,
        id: &str,
        run: &str,
        number: u32,
        end: &JobResult,
    ) -> std::io::Result<()> {
        let path = self.attempt_dir(id, run, number).join("result.json");
        let mut result: AttemptResult = read_json(&path)?.unwrap_or_default();
        result.ended = Some(end.clone());
        write_json(&path, &result)
    }

    /// Read what a gone attempt's directory holds.
    pub fn observe(&self, id: &str, run: &str, number: u32) -> std::io::Result<Observed> {
        let dir = self.attempt_dir(id, run, number);
        let result: AttemptResult = read_json(&dir.join("result.json"))?.unwrap_or_default();
        let cancel: Option<CancelMarker> = read_json(&dir.join("cancel.json"))?;
        let (counts, recovery_completed) = match std::fs::read_to_string(dir.join("tlc.log")) {
            Ok(log) => (
                crate::tlc::summary(&log).counts,
                log.lines().any(|l| crate::tlc::recovered(l).is_some()),
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, false),
            Err(e) => return Err(e),
        };
        let checkpoint = self.checkpoint(id, run)?;
        // On disk still: tlx deletes its own once its search ends.
        let on_disk = engine_checkpoint(&self.checkpoint_dir(id, run).join("tlc"));
        let kept = on_disk
            && checkpoint
                .as_ref()
                .is_some_and(|c| c.attempt == Some(number));
        // A resume's: the run's checkpoint, kept by an earlier attempt.
        let recovered = on_disk && checkpoint.is_some_and(|c| c.attempt.is_none_or(|a| a < number));
        Ok(Observed {
            started: dir.join("started").exists(),
            cancel: cancel.map(|c| (c.by, c.replaced_by)),
            finished: result.finished,
            paused: dir.join("paused").exists(),
            kept,
            recovered,
            recovery_completed,
            signal: result.signal,
            out_of_memory: result.out_of_memory,
            no_checkpoint: dir.join("no-checkpoint").exists(),
            timed_out: dir.join("timed-out").exists(),
            duration_secs: result.duration_secs,
            lanes: result.lanes,
            counts,
        })
    }

    /// Write the page's pause request in an attempt's directory.
    pub fn write_pause(&self, id: &str, run: &str, number: u32) -> std::io::Result<()> {
        write_file(&self.attempt_dir(id, run, number).join("pause"), b"")
    }

    /// Remove the page's pause request from an attempt's directory, as a pause
    /// that could not be carried out leaves it.
    pub fn remove_pause(&self, id: &str, run: &str, number: u32) -> std::io::Result<()> {
        match std::fs::remove_file(self.attempt_dir(id, run, number).join("pause")) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Append `event` (`freeze`, `thaw <slot>` or `unpause`), at `now`, to an
    /// attempt's `frozen.log`.
    pub fn log_frozen(
        &self,
        id: &str,
        run: &str,
        number: u32,
        event: &str,
        now: u64,
    ) -> std::io::Result<()> {
        use std::io::Write;
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.attempt_dir(id, run, number).join("frozen.log"))?;
        writeln!(log, "{now} {event}")
    }

    /// An attempt's `frozen.log`, empty if it was never frozen.
    pub fn frozen_log(&self, id: &str, run: &str, number: u32) -> std::io::Result<String> {
        match std::fs::read_to_string(self.attempt_dir(id, run, number).join("frozen.log")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            read => read,
        }
    }

    /// Whether the P slot is open: a human may close it from the page, and
    /// the mark outlives restarts and reboots.
    pub fn p_open(&self) -> bool {
        !self.root.join("p-closed").exists()
    }

    /// Open or close the P slot.
    ///
    /// # Errors
    /// If the mark cannot be written or removed.
    pub fn set_p_open(&self, open: bool) -> std::io::Result<()> {
        let mark = self.root.join("p-closed");
        if !open {
            return write_file(&mark, b"");
        }
        match std::fs::remove_file(&mark) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Write a cancel marker in an attempt's directory.
    pub fn write_cancel(
        &self,
        id: &str,
        run: &str,
        number: u32,
        marker: &CancelMarker,
    ) -> std::io::Result<()> {
        write_json(
            &self.attempt_dir(id, run, number).join("cancel.json"),
            marker,
        )
    }

    /// Delete an attempt's directory: an undone attempt never ran.
    pub fn delete_attempt(&self, id: &str, run: &str, number: u32) -> std::io::Result<()> {
        remove_dir(&self.attempt_dir(id, run, number))
    }

    /// Delete an ended attempt's scratch, keeping its markers and `result.json`.
    pub fn delete_scratch(&self, id: &str, run: &str, number: u32) -> std::io::Result<()> {
        remove_dir(&self.attempt_dir(id, run, number).join("scratch"))
    }

    /// A run's checkpoint directory.
    pub fn checkpoint_dir(&self, id: &str, run: &str) -> PathBuf {
        self.root.join("checkpoints").join(id).join(run)
    }

    /// The checkpoint a run's next attempt recovers from: its record, if TLC's
    /// checkpoint is there too.
    ///
    /// # Errors
    /// If the record cannot be read.
    pub fn checkpoint(&self, id: &str, run: &str) -> std::io::Result<Option<Checkpoint>> {
        let dir = self.checkpoint_dir(id, run);
        if !dir.join("tlc").is_dir() {
            return Ok(None);
        }
        read_json(&dir.join("checkpoint.json"))
    }

    /// Delete an ended run's checkpoint, so checkpoints never pile up.
    pub fn delete_checkpoint(&self, id: &str, run: &str) -> std::io::Result<()> {
        remove_dir(&self.checkpoint_dir(id, run))
    }

    /// The scratch directories no live attempt in `state` owns, and the run
    /// checkpoints no run that has not ended owns: left by a crash, or by a
    /// run gone from the state. Settling deletes those of the runs it knows.
    ///
    /// # Errors
    /// If a directory cannot be listed.
    pub fn orphans(&self, state: &State) -> std::io::Result<Vec<PathBuf>> {
        let run = |id: &str, name: &str| state.runs.get(id)?.iter().find(|r| r.name == name);
        let mut found = Vec::new();
        for id in names_in(&self.root.join("attempts"))? {
            for name in names_in(&self.root.join("attempts").join(&id))? {
                for n in names_in(&self.root.join("attempts").join(&id).join(&name))? {
                    let live = run(&id, &name)
                        .and_then(Run::live_attempt)
                        .is_some_and(|a| a.number.to_string() == n);
                    let scratch = self
                        .root
                        .join("attempts")
                        .join(&id)
                        .join(&name)
                        .join(&n)
                        .join("scratch");
                    if !live && scratch.is_dir() {
                        found.push(scratch);
                    }
                }
            }
        }
        for id in names_in(&self.root.join("checkpoints"))? {
            for name in names_in(&self.root.join("checkpoints").join(&id))? {
                let kept = run(&id, &name).is_some_and(|r| !matches!(r.phase, Phase::Ended { .. }));
                if !kept {
                    found.push(self.checkpoint_dir(&id, &name));
                }
            }
        }
        Ok(found)
    }

    /// Delete the [`Self::orphans`] of `state`. Returns the bytes freed.
    ///
    /// # Errors
    /// If one cannot be listed or deleted.
    pub fn delete_orphans(&self, state: &State) -> std::io::Result<u64> {
        let mut freed = 0;
        for dir in self.orphans(state)? {
            freed += dir_bytes(&dir);
            remove_dir(&dir)?;
        }
        Ok(freed)
    }
}

/// The names in a directory; none if it is missing.
fn names_in(dir: &Path) -> std::io::Result<Vec<String>> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// The disk the files under `dir` take, in bytes, each file once however many
/// links it has (tlx's checkpoint links the spill files it shares); none if
/// missing. Allocated blocks, as `du` counts: a sparse spill file's length
/// overstates it by its holes.
pub fn dir_bytes(dir: &Path) -> u64 {
    fn walk(dir: &Path, seen: &mut std::collections::HashSet<(u64, u64)>) -> u64 {
        use std::os::unix::fs::MetadataExt;
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        entries
            .flatten()
            .map(|e| match e.metadata() {
                Ok(m) if m.is_dir() => walk(&e.path(), seen),
                Ok(m) if seen.insert((m.dev(), m.ino())) => m.blocks() * 512,
                _ => 0,
            })
            .sum()
    }
    walk(dir, &mut std::collections::HashSet::new())
}

/// A run's checkpoint, which its next attempt recovers from with `-recover`:
/// the engine's checkpoint directory is `tlc/` beside this record's
/// `checkpoint.json` (TLC's run directory, or tlx's metadir). The engine goes
/// on checkpointing into it, and deletes it once it finishes. Which engine
/// wrote it is the run's, from its `job.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// The workers it ran with: TLC's recovery needs as many, one file each;
    /// tlx's takes any number.
    pub workers: u64,
    /// TLC's fingerprint function (`-fp`), as its log's start says: the
    /// recovered fingerprints were taken with it, so new states must be too.
    /// `None` for tlx.
    pub fp: Option<u64>,
    /// The run's generated states at the checkpoint, its earlier recoveries
    /// counted: TLC's own count starts again from the states it recovers.
    #[serde(default)]
    pub generated: Option<u64>,
    /// The attempt that kept it, for a pause: settling counts that attempt
    /// paused only if its own checkpoint was kept.
    #[serde(default)]
    pub attempt: Option<u32>,
}

/// The seconds an attempt was frozen between `since` and `now`, from its
/// `frozen.log`: from each freeze to the thaw or unpause after it, or to `now`.
pub fn frozen_secs(log: &str, since: u64, now: u64) -> u64 {
    let mut total = 0;
    let mut from: Option<u64> = None;
    for line in log.lines() {
        let mut words = line.split_whitespace();
        let (Some(Ok(at)), Some(event)) = (words.next().map(str::parse::<u64>), words.next())
        else {
            continue;
        };
        let at = at.clamp(since, now);
        match (event, from) {
            ("freeze", None) => from = Some(at),
            ("thaw" | "unpause", Some(f)) => {
                total += at.saturating_sub(f);
                from = None;
            }
            _ => {}
        }
    }
    total + from.map_or(0, |f| now.saturating_sub(f))
}

/// Whether a kept checkpoint's `tlc` directory still holds the engine's
/// checkpoint: tlx's `checkpoint/MANIFEST`, or TLC's `queue.chkpt`.
fn engine_checkpoint(dir: &Path) -> bool {
    dir.join("checkpoint/MANIFEST").is_file() || dir.join("queue.chkpt").is_file()
}

/// Remove a directory and what it holds; one already gone is fine.
fn remove_dir(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_round_trips_under_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let lock = store.lock().unwrap();
        assert_eq!(store.load(&lock).unwrap(), State::default());
        let mut state = State::default();
        state.numbered.insert("a".to_owned(), 2);
        store.save(&lock, &state).unwrap();
        assert_eq!(store.load(&lock).unwrap(), state);
    }

    #[test]
    fn orphans_are_scratch_and_checkpoints_no_live_run_owns() {
        use crate::runner::state::tests::{run, running};
        use crate::runner::state::{Attempt, Slot};
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        // Run a of job "a" is live in attempt 2; job "b" ended; "ghost" is in no state.
        let mut state = State::default();
        let mut a = run("v1", running(), "A.cfg");
        a.attempts = vec![
            Attempt {
                ended: true,
                ..Attempt::new(1, Slot::E, false)
            },
            Attempt::new(2, Slot::E, false),
        ];
        state.runs.insert("a".to_owned(), vec![a]);
        let ended = Phase::Ended {
            result: State::outcome_only(crate::outcome::Outcome::Ok),
        };
        state
            .runs
            .insert("b".to_owned(), vec![run("v1", ended, "B.cfg")]);
        let scratch = |id: &str, n: u32| store.attempt_dir(id, "v1", n).join("scratch");
        for (id, n) in [("a", 1), ("a", 2), ("ghost", 1)] {
            std::fs::create_dir_all(scratch(id, n)).unwrap();
            std::fs::write(scratch(id, n).join("fpset.spill"), [0u8; 10]).unwrap();
        }
        for id in ["a", "b", "ghost"] {
            std::fs::create_dir_all(store.checkpoint_dir(id, "v1").join("tlc")).unwrap();
        }
        let mut found = store.orphans(&state).unwrap();
        found.sort();
        let mut want = vec![
            scratch("a", 1),
            scratch("ghost", 1),
            store.checkpoint_dir("b", "v1"),
            store.checkpoint_dir("ghost", "v1"),
        ];
        want.sort();
        assert_eq!(found, want);
        // Each 10-byte file takes one block.
        let block = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(scratch("a", 2).join("fpset.spill"))
                .unwrap()
                .blocks()
                * 512
        };
        assert_eq!(store.delete_orphans(&state).unwrap(), 2 * block);
        assert!(scratch("a", 2).is_dir() && store.checkpoint_dir("a", "v1").is_dir());
        assert_eq!(store.orphans(&state).unwrap(), Vec::<PathBuf>::new());
        assert_eq!(dir_bytes(&dir.path().join("missing")), 0);
        // A file linked twice (a checkpoint sharing a spill file) counts once.
        let live = scratch("a", 2);
        std::fs::create_dir(live.join("checkpoint")).unwrap();
        std::fs::hard_link(live.join("fpset.spill"), live.join("checkpoint/fpset-0")).unwrap();
        assert_eq!(dir_bytes(&live), block);
        // A sparse file counts its blocks, not its length.
        std::fs::File::create(live.join("fpset-1"))
            .unwrap()
            .set_len(64 << 20)
            .unwrap();
        assert_eq!(dir_bytes(&live), block);
        // A file where a directory should be cannot be listed.
        let file = scratch("a", 2).join("fpset.spill");
        names_in(&file).unwrap_err();
    }

    #[test]
    fn a_checkpoint_counts_only_with_tlc_s_directory_and_is_deleted_whole() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.checkpoint("a", "v1").unwrap(), None);
        let at = store.checkpoint_dir("a", "v1");
        let record = Checkpoint {
            workers: 31,
            fp: Some(83),
            generated: None,
            attempt: None,
        };
        std::fs::create_dir_all(&at).unwrap();
        write_json(&at.join("checkpoint.json"), &record).unwrap();
        assert_eq!(store.checkpoint("a", "v1").unwrap(), None);
        std::fs::create_dir(at.join("tlc")).unwrap();
        assert_eq!(store.checkpoint("a", "v1").unwrap(), Some(record));
        store.delete_checkpoint("a", "v1").unwrap();
        store.delete_checkpoint("a", "v1").unwrap();
        assert!(!at.exists());
    }

    #[test]
    fn a_tarball_is_kept_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let lock = store.lock().unwrap();
        store.put_tarball(&lock, "a", b"one").unwrap();
        store.put_tarball(&lock, "a", b"two").unwrap();
        assert_eq!(std::fs::read(store.tarball("a")).unwrap(), b"one");
    }

    #[test]
    fn an_attempt_directory_is_observed_marker_by_marker() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.observe("a", "v1", 1).unwrap(), Observed::default());
        let first = AttemptResult {
            duration_secs: 30,
            ..AttemptResult::default()
        };
        store.create_attempt("a", "v1", 1, &first).unwrap();
        let attempt = store.attempt_dir("a", "v1", 1);
        for marker in ["started", "paused", "no-checkpoint", "timed-out"] {
            std::fs::write(attempt.join(marker), "").unwrap();
        }
        let cancel = CancelMarker {
            by: "ui".to_owned(),
            replaced_by: None,
        };
        store.write_cancel("a", "v1", 1, &cancel).unwrap();
        let seen = store.observe("a", "v1", 1).unwrap();
        assert!(seen.started && seen.paused && seen.no_checkpoint && seen.timed_out);
        assert_eq!(
            (seen.cancel, seen.duration_secs, seen.kept),
            (Some(("ui".to_owned(), None)), 30, false)
        );
        // Kept only when the run's checkpoint is this attempt's, and on disk.
        let at = store.checkpoint_dir("a", "v1");
        std::fs::create_dir_all(at.join("tlc")).unwrap();
        std::fs::write(at.join("tlc/queue.chkpt"), "").unwrap();
        for (attempt, kept) in [(Some(2), false), (Some(1), true)] {
            let record = Checkpoint {
                workers: 16,
                fp: None,
                generated: None,
                attempt,
            };
            write_json(&at.join("checkpoint.json"), &record).unwrap();
            assert_eq!(store.observe("a", "v1", 1).unwrap().kept, kept);
        }
        std::fs::remove_file(at.join("tlc/queue.chkpt")).unwrap();
        assert!(!store.observe("a", "v1", 1).unwrap().kept);
        // Recovered by a later attempt only while the engine's checkpoint is there.
        let recovered = || store.observe("a", "v1", 2).unwrap().recovered;
        assert!(!recovered());
        for file in ["checkpoint/MANIFEST", "queue.chkpt"] {
            let path = at.join("tlc").join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
            assert!(recovered(), "{file}");
            assert!(!store.observe("a", "v1", 1).unwrap().recovered, "{file}");
            std::fs::remove_file(&path).unwrap();
        }
        store.delete_scratch("a", "v1", 1).unwrap();
        assert!(!attempt.join("scratch").exists() && attempt.join("result.json").exists());
        store.delete_attempt("a", "v1", 1).unwrap();
        store.delete_attempt("a", "v1", 1).unwrap();
        assert!(!attempt.exists());
        std::fs::write(dir.path().join("attempts/a/v1/1"), "a file").unwrap();
        store.delete_attempt("a", "v1", 1).unwrap_err();
    }

    #[test]
    fn frozen_time_runs_from_each_freeze_to_its_thaw_or_unpause() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store
            .create_attempt("a", "v1", 1, &AttemptResult::default())
            .unwrap();
        assert_eq!(store.frozen_log("a", "v1", 1).unwrap(), "");
        for (event, at) in [("freeze", 100), ("thaw e", 160), ("freeze", 200)] {
            store.log_frozen("a", "v1", 1, event, at).unwrap();
        }
        let log = store.frozen_log("a", "v1", 1).unwrap();
        // Still frozen at 250: 60 + 50.
        assert_eq!(frozen_secs(&log, 0, 250), 110);
        store.log_frozen("a", "v1", 1, "unpause", 230).unwrap();
        let log = store.frozen_log("a", "v1", 1).unwrap();
        assert_eq!(frozen_secs(&log, 0, 250), 90);
        // Before the attempt's start counts for nothing; a line unread is skipped.
        assert_eq!(frozen_secs(&format!("{log}junk\n"), 130, 250), 60);
        assert_eq!(frozen_secs("5 thaw p\n", 0, 9), 0);
    }

    #[test]
    fn the_p_slot_closes_and_opens_and_a_pause_is_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert!(store.p_open());
        store.set_p_open(false).unwrap();
        store.set_p_open(false).unwrap();
        assert!(!store.p_open());
        store.set_p_open(true).unwrap();
        store.set_p_open(true).unwrap();
        assert!(store.p_open());
        store
            .create_attempt("a", "v1", 1, &AttemptResult::default())
            .unwrap();
        store.write_pause("a", "v1", 1).unwrap();
        let pause = store.attempt_dir("a", "v1", 1).join("pause");
        assert!(pause.exists());
        // Removed, and removed again; one that cannot be removed says so.
        store.remove_pause("a", "v1", 1).unwrap();
        store.remove_pause("a", "v1", 1).unwrap();
        assert!(!pause.exists());
        std::fs::create_dir(&pause).unwrap();
        assert!(store.remove_pause("a", "v1", 1).is_err());
    }

    #[test]
    fn the_lock_excludes_another_holder() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let held = store.lock().unwrap();
        let other = File::create(dir.path().join("admission.lock")).unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(held);
        other.try_lock().unwrap();
    }
}

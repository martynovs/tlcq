//! The laptop's state (design D6, D13): job records, follows, the cache, and the
//! locks that order their writes.
//!
//! ```text
//! <state>/placement.lock      the placement lock
//! <state>/jobs/<id>/record.json (its state, entries and `job.json`), job.tar, lock
//! <state>/jobs/<id>/<run>/     a laptop run's markers, results, log and scratch
//! <state>/tickets/<n>-<id>    the laptop slot's tickets, in submit order
//! <state>/follows/<n>.json    one per entry an agent placed
//! <state>/results/<id>.json   the cache, created exclusively
//! <state>/history.jsonl       the history (D5)
//! <state>/curves/<id>-<run>-<attempt>.jsonl  each ended attempt's progress curve (D15; `.tsv` from before)
//! ```
//!
//! Every write of a record or a follow takes a [`PlacementLock`], so the type
//! system keeps the rule that they happen under it. Files are written whole, to a
//! temporary name renamed into place, so a reader never sees half of one.

use std::fs::File;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::files::{read_json, to_json, write_file, write_json};

use crate::entry::{Entry, Key, Label};
use crate::outcome::JobResult;
use crate::proto::JobInfo;
pub use crate::proto::RunRef;

/// A job record's place in its life (D6):
///
/// ```text
/// laptop ──▶ ended               (a laptop job)
///    └──▶ moving ──▶ vm ──▶ ended
/// vm ──▶ ended                    (a job sent to the VM)
/// sent? ──▶ vm ──▶ ended          (a commit whose outcome is unknown)
///    └──▶ never sent
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Placed on the laptop, waiting for the slot or running.
    Laptop,
    /// Being moved to the VM after a laptop timeout.
    Moving,
    /// On the VM.
    Vm,
    /// Sent to the VM, the outcome of the commit unknown (written ahead of it).
    Sent,
    /// Ended; the outcome is in the record.
    Ended,
}

/// An entry attached while the VM was unreachable, for the next commit to flush.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// The entry.
    pub entry: Entry,
    /// The run it attached to.
    pub run: RunRef,
}

/// The laptop's record of a job it placed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    /// The job's id.
    pub id: String,
    /// Its state.
    pub state: State,
    /// The run the record follows, when known: a new job's write-ahead leaves it
    /// empty, an attach's keeps the run it attaches to, so a send that applied
    /// nothing is never taken for an older run of the id.
    pub run: Option<String>,
    /// How many laptop runs this id has had, for naming the next `l<n>`.
    pub laptop_runs: u32,
    /// The token of the commit or move whose outcome is unknown (`sent`, `moving`).
    pub token: Option<String>,
    /// Entries of a laptop job.
    pub entries: Vec<Entry>,
    /// The laptop's read-only mirror of a VM job's entries.
    pub mirror: Vec<Entry>,
    /// Entries attached while the VM was unreachable.
    pub pending: Vec<Pending>,
    /// The result, once ended.
    pub result: Option<JobResult>,
    /// Bumped by every write, so a poll can tell whether a record changed under it.
    pub generation: u64,
    /// The job's description (`job.json`), set by the command that placed it.
    pub info: Option<JobInfo>,
    /// The laptop run's last awake time, in seconds, as its supervisor last wrote
    /// it: the lower bound of a run that ends early, a failed move's included.
    pub elapsed_secs: u64,
    /// Earlier VM runs of this id, replaced by a new placement before they
    /// ended (a run still stopping): their ends still go to the history.
    #[serde(default)]
    pub ending: Vec<String>,
}

impl JobRecord {
    /// A fresh record of `id` in `state`.
    pub fn new(id: &str, state: State) -> Self {
        Self {
            id: id.to_owned(),
            state,
            run: None,
            laptop_runs: 0,
            token: None,
            entries: Vec::new(),
            mirror: Vec::new(),
            pending: Vec::new(),
            result: None,
            generation: 0,
            info: None,
            elapsed_secs: 0,
            ending: Vec::new(),
        }
    }

    /// Follow the VM run `run` from now on. The run it replaces, if any and
    /// another, is not known to have ended: it is asked about until its end
    /// reaches the history (D5).
    pub fn take_run(&mut self, run: Option<String>) {
        if run != self.run {
            if let Some(old) = self.run.take() {
                if !self.ending.contains(&old) {
                    self.ending.push(old);
                }
            }
        }
        self.run = run;
    }
}

/// What a follow follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Followed {
    /// A run, by name, or a commit's token until it resolves.
    Run(RunRef),
    /// A cache hit's result.
    Result(JobResult),
    /// A commit that reached no run.
    NeverSent,
}

/// What an agent's reports follow for one entry it placed (D6, Follows).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Follow {
    /// Fixed when it is created; the latest per key has the highest.
    pub number: u64,
    /// The configuration placed.
    pub key: Key,
    /// The agent that placed it.
    pub label: Label,
    /// The job's id.
    pub id: String,
    /// What it follows.
    pub what: Followed,
}

/// The placement lock, held while the guard lives.
#[derive(Debug)]
pub struct PlacementLock {
    _file: File,
}

/// The laptop's state directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Open the state directory at `root`, creating what is missing.
    pub fn open(root: &Path) -> std::io::Result<Self> {
        for dir in ["jobs", "follows", "results", "tickets", "curves"] {
            std::fs::create_dir_all(root.join(dir))?;
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// The history's path.
    pub fn history(&self) -> PathBuf {
        self.root.join("history.jsonl")
    }

    /// Take the placement lock, waiting for it.
    pub fn placement_lock(&self) -> std::io::Result<PlacementLock> {
        let file = File::create(self.root.join("placement.lock"))?;
        file.lock()?;
        Ok(PlacementLock { _file: file })
    }

    /// A job's directory.
    pub fn job_dir(&self, id: &str) -> PathBuf {
        self.root.join("jobs").join(id)
    }

    /// The record of `id`, if the laptop has one.
    pub fn record(&self, id: &str) -> std::io::Result<Option<JobRecord>> {
        read_json(&self.job_dir(id).join("record.json"))
    }

    /// Every job record.
    pub fn records(&self) -> std::io::Result<Vec<JobRecord>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.root.join("jobs"))? {
            if let Some(r) = read_json(&entry?.path().join("record.json"))? {
                out.push(r);
            }
        }
        out.sort_by(|a: &JobRecord, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Write `record`, bumping its generation.
    pub fn write_record(
        &self,
        _lock: &PlacementLock,
        record: &mut JobRecord,
    ) -> std::io::Result<()> {
        record.generation += 1;
        let dir = self.job_dir(&record.id);
        std::fs::create_dir_all(&dir)?;
        write_json(&dir.join("record.json"), record)
    }

    /// Remove the record of `id` (an `invalid` answer undoing a write-ahead).
    pub fn remove_record(&self, _lock: &PlacementLock, id: &str) -> std::io::Result<()> {
        match std::fs::remove_dir_all(self.job_dir(id)) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// A laptop run's directory: its markers, `result.json`, `progress.json`,
    /// TLC's log, the extracted job and its scratch (D7).
    pub fn run_dir(&self, id: &str, run: &str) -> PathBuf {
        self.job_dir(id).join(run)
    }

    /// A laptop job's tarball.
    pub fn tarball(&self, id: &str) -> PathBuf {
        self.job_dir(id).join("job.tar")
    }

    /// Take the job lock of `id`, waiting for it. Its supervisor holds it for
    /// the job's life (D7).
    pub fn job_lock(&self, id: &str) -> std::io::Result<File> {
        let dir = self.job_dir(id);
        std::fs::create_dir_all(&dir)?;
        let file = File::options()
            .create(true)
            .append(true)
            .open(dir.join("lock"))?;
        file.lock()?;
        Ok(file)
    }

    /// Write the slot ticket of `id`, after every other: the laptop's slot is
    /// taken in submit order (D7).
    pub fn write_ticket(&self, _lock: &PlacementLock, id: &str) -> std::io::Result<()> {
        let n = self.tickets()?.last().map_or(1, |(n, _)| n + 1);
        write_file(&self.root.join("tickets").join(format!("{n}-{id}")), b"")
    }

    /// Every slot ticket, in order: its number and its job.
    pub fn tickets(&self) -> std::io::Result<Vec<(u64, String)>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.root.join("tickets"))? {
            let name = entry?.file_name();
            let ticket = name.to_str().and_then(|n| n.split_once('-'));
            if let Some((n, id)) = ticket.and_then(|(n, id)| Some((n.parse().ok()?, id))) {
                out.push((n, id.to_owned()));
            }
        }
        out.sort();
        Ok(out)
    }

    /// Remove the slot ticket of `id`, if it has one.
    pub fn remove_ticket(&self, id: &str) -> std::io::Result<()> {
        for (n, _) in self.tickets()?.into_iter().filter(|(_, t)| t == id) {
            std::fs::remove_file(self.root.join("tickets").join(format!("{n}-{id}")))?;
        }
        Ok(())
    }

    /// The progress curve of attempt `attempt` of run `run` of `id`.
    pub fn curve_path(&self, id: &str, run: &str, attempt: u32) -> PathBuf {
        self.root
            .join("curves")
            .join(format!("{id}-{run}-{attempt}.jsonl"))
    }

    /// The curves of run `run` of `id`, by attempt: their file names.
    ///
    /// # Errors
    /// If the directory cannot be read.
    pub fn curves_of(&self, id: &str, run: &str) -> std::io::Result<Vec<String>> {
        let prefix = format!("{id}-{run}-");
        let mut out: Vec<(u32, String)> = Vec::new();
        for entry in std::fs::read_dir(self.root.join("curves"))? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            let attempt = name
                .strip_prefix(&prefix)
                .and_then(|rest| {
                    rest.strip_suffix(".jsonl")
                        .or_else(|| rest.strip_suffix(".tsv"))
                })
                .and_then(|n| n.parse().ok());
            if let Some(n) = attempt {
                out.push((n, name));
            }
        }
        out.sort();
        Ok(out.into_iter().map(|(_, name)| name).collect())
    }

    /// A curve file by its name, as the history points at it.
    pub fn curve_file(&self, name: &str) -> PathBuf {
        self.root.join("curves").join(name)
    }

    /// Free disk under the state directory, in bytes.
    pub fn free_bytes(&self) -> std::io::Result<u64> {
        let fs = rustix::fs::statvfs(&self.root)?;
        Ok(fs.f_bavail.saturating_mul(fs.f_frsize))
    }

    /// Whether a supervisor holds the job lock of `id`: tried, never waited on.
    pub fn job_lock_held(&self, id: &str) -> std::io::Result<bool> {
        let path = self.job_dir(id).join("lock");
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        match file.try_lock() {
            Ok(()) => Ok(false),
            Err(std::fs::TryLockError::WouldBlock) => Ok(true),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    /// Every follow, by number.
    pub fn follows(&self) -> std::io::Result<Vec<Follow>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.root.join("follows"))? {
            // Only `<n>.json`: a `.tmp` left by a write cut off is no follow.
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Some(f) = read_json(&path)? {
                    out.push(f);
                }
            }
        }
        out.sort_by_key(|f: &Follow| f.number);
        Ok(out)
    }

    /// The number the next follow gets: one past the highest.
    pub fn next_follow_number(&self, _lock: &PlacementLock) -> std::io::Result<u64> {
        Ok(self.follows()?.last().map_or(1, |f| f.number + 1))
    }

    /// Write `follow` (a new one, or a rewrite of its target).
    pub fn write_follow(&self, _lock: &PlacementLock, follow: &Follow) -> std::io::Result<()> {
        write_json(&self.follow_path(follow.number), follow)
    }

    /// Remove the follow numbered `number`.
    pub fn remove_follow(&self, _lock: &PlacementLock, number: u64) -> std::io::Result<()> {
        match std::fs::remove_file(self.follow_path(number)) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    fn follow_path(&self, number: u64) -> PathBuf {
        self.root.join("follows").join(format!("{number}.json"))
    }

    /// The cached result of `id`, if any.
    pub fn cached(&self, id: &str) -> std::io::Result<Option<JobResult>> {
        read_json(&self.cache_path(id))
    }

    /// Cache `result` for `id`, unless a result is there already: the first one
    /// wins. Returns whether it was written.
    pub fn cache(&self, id: &str, result: &JobResult) -> std::io::Result<bool> {
        let final_path = self.cache_path(id);
        let tmp = final_path.with_extension("tmp");
        write_file(&tmp, &to_json(result)?)?;
        // A hard link fails if the target exists, so the first result stays.
        let linked = std::fs::hard_link(&tmp, &final_path);
        std::fs::remove_file(&tmp)?;
        match linked {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn cache_path(&self, id: &str) -> PathBuf {
        self.root.join("results").join(format!("{id}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::Outcome;

    fn key(cfg: &str) -> Key {
        Key {
            worktree: PathBuf::from("/w"),
            model_dir: PathBuf::from("tla/m"),
            spec: "M".to_owned(),
            cfg_name: cfg.to_owned(),
        }
    }

    fn label() -> Label {
        Label {
            project: "p".to_owned(),
            worktree: PathBuf::from("/w"),
            branch: None,
            agent: None,
        }
    }

    fn result(outcome: Outcome) -> JobResult {
        JobResult {
            outcome,
            exit_status: Some(0),
            distinct: None,
            generated: None,
            depth: None,
            place: "laptop".to_owned(),
            duration_secs: 1,
            lanes: Vec::new(),
            errors: Vec::new(),
            note: None,
        }
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn records_round_trip_and_bump_their_generation() {
        let (dir, s) = store();
        let lock = s.placement_lock().unwrap();
        assert_eq!(s.record("a").unwrap(), None);
        let mut r = JobRecord::new("a", State::Sent);
        r.token = Some("t".to_owned());
        r.mirror.push(Entry {
            key: key("A.cfg"),
            label: label(),
        });
        r.pending.push(Pending {
            entry: Entry {
                key: key("A.cfg"),
                label: label(),
            },
            run: RunRef::Token("t".to_owned()),
        });
        s.write_record(&lock, &mut r).unwrap();
        s.write_record(&lock, &mut r).unwrap();
        assert_eq!(r.generation, 2);
        assert_eq!(s.record("a").unwrap(), Some(r.clone()));
        let mut b = JobRecord::new("b", State::Laptop);
        s.write_record(&lock, &mut b).unwrap();
        assert_eq!(s.records().unwrap(), [r, b]);
        s.remove_record(&lock, "a").unwrap();
        s.remove_record(&lock, "a").unwrap();
        assert_eq!(s.record("a").unwrap(), None);
        assert_eq!(s.history(), dir.path().join("history.jsonl"));
    }

    #[test]
    fn unreadable_files_are_errors() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, s) = store();
        std::fs::create_dir_all(s.job_dir("x").join("record.json")).unwrap();
        s.record("x").unwrap_err();
        let lock = s.job_dir("x").join("lock");
        File::create(&lock).unwrap();
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o000)).unwrap();
        s.job_lock_held("x").unwrap_err();
    }

    #[test]
    fn a_corrupt_record_is_an_error() {
        let (_dir, s) = store();
        std::fs::create_dir_all(s.job_dir("x")).unwrap();
        std::fs::write(s.job_dir("x").join("record.json"), "{").unwrap();
        assert_eq!(s.record("x").unwrap_err().kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn follows_are_numbered_rewritten_and_removed() {
        let (dir, s) = store();
        let lock = s.placement_lock().unwrap();
        assert_eq!(s.next_follow_number(&lock).unwrap(), 1);
        let mut f = Follow {
            number: 1,
            key: key("A.cfg"),
            label: label(),
            id: "a".to_owned(),
            what: Followed::Run(RunRef::Token("t".to_owned())),
        };
        s.write_follow(&lock, &f).unwrap();
        f.what = Followed::Run(RunRef::Run("v1".to_owned()));
        s.write_follow(&lock, &f).unwrap();
        let g = Follow {
            number: s.next_follow_number(&lock).unwrap(),
            what: Followed::Result(result(Outcome::Ok)),
            ..f.clone()
        };
        s.write_follow(&lock, &g).unwrap();
        std::fs::write(dir.path().join("follows").join("3.tmp"), "{").unwrap();
        assert_eq!(s.follows().unwrap(), [f, g.clone()]);
        s.remove_follow(&lock, 1).unwrap();
        s.remove_follow(&lock, 1).unwrap();
        assert_eq!(s.follows().unwrap(), [g]);
    }

    #[test]
    fn the_first_cached_result_wins() {
        let (_dir, s) = store();
        assert_eq!(s.cached("a").unwrap(), None);
        assert!(s.cache("a", &result(Outcome::Ok)).unwrap());
        assert!(!s.cache("a", &result(Outcome::Unexpected)).unwrap());
        assert_eq!(s.cached("a").unwrap(), Some(result(Outcome::Ok)));
    }

    #[test]
    fn a_job_lock_is_tried_never_waited_on() {
        let (_dir, s) = store();
        assert!(!s.job_lock_held("a").unwrap());
        std::fs::create_dir_all(s.job_dir("a")).unwrap();
        let held = File::create(s.job_dir("a").join("lock")).unwrap();
        assert!(!s.job_lock_held("a").unwrap());
        held.lock().unwrap();
        assert!(s.job_lock_held("a").unwrap());
        drop(held);
        assert!(!s.job_lock_held("a").unwrap());
    }

    #[test]
    fn tickets_go_in_order_and_are_removed_by_job() {
        let (dir, s) = store();
        let lock = s.placement_lock().unwrap();
        s.write_ticket(&lock, "b").unwrap();
        s.write_ticket(&lock, "a").unwrap();
        std::fs::write(dir.path().join("tickets").join("x.tmp"), "").unwrap();
        let ids = |s: &Store| -> Vec<(u64, String)> { s.tickets().unwrap() };
        assert_eq!(ids(&s), [(1, "b".to_owned()), (2, "a".to_owned())]);
        s.remove_ticket("b").unwrap();
        s.remove_ticket("b").unwrap();
        s.write_ticket(&lock, "c").unwrap();
        assert_eq!(ids(&s), [(2, "a".to_owned()), (3, "c".to_owned())]);
        assert!(s.free_bytes().unwrap() > 0);
        assert_eq!(s.run_dir("a", "l1"), s.job_dir("a").join("l1"));
        assert_eq!(s.tarball("a"), s.job_dir("a").join("job.tar"));
    }

    #[test]
    fn a_job_lock_taken_is_held_until_dropped() {
        let (_dir, s) = store();
        let held = s.job_lock("a").unwrap();
        assert!(s.job_lock_held("a").unwrap());
        drop(held);
        assert!(!s.job_lock_held("a").unwrap());
    }

    #[test]
    fn the_placement_lock_excludes_another_holder() {
        let (dir, s) = store();
        let held = s.placement_lock().unwrap();
        let other = File::create(dir.path().join("placement.lock")).unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(held);
        other.try_lock().unwrap();
    }
}

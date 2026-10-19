//! The laptop supervisor, `tlcq supervise <id> <run>` (design D7): one per
//! laptop job, detached, holding the job's lock from its launch to its end. It
//! waits for the slot (by ticket, in submit order) and for free disk, runs TLC
//! on the extracted job, stops it at the cap, and ends the job: with TLC's
//! verdict, `timeout`, or with `--vm-on-timeout` a move to the VM queue.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::commit::{self, CommitJob, CommitReply, CommitRequest, CommitRoute, JobAnswer};
use crate::curve::{self, Curve, Recorder};
use crate::files::{write_file, write_json};
use crate::history::Expected;
use crate::job::Limits;
use crate::laptop_runs::{clean, keep_curve, laptop_result, Phase, Progress};
use crate::outcome::{JobResult, Outcome};
use crate::proto::{Counters, JobInfo};
use crate::remote::Remote;
use crate::runner::exec::{
    end_curve, engine_command, extract, finished, Clock, Install, Resources, Tail,
};
use crate::snapshot::{self, Error};
use crate::store::{JobRecord, State, Store};
use crate::tlc;

/// The laptop's slot, TLC's resources and the cap (D7, D13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Jobs that run at once.
    pub slots: usize,
    /// TLC's resources.
    pub resources: Resources,
    /// The cap on a run that is not `--local`, in seconds of awake time.
    pub cap_secs: u64,
    /// Free disk, in bytes, below which TLC does not start.
    pub disk_floor: u64,
    /// Seconds between looks at the slot, the disk and TLC.
    pub poll_secs: u64,
    /// The tarball limits, as the placing command checked them.
    pub limits: Limits,
}

/// One laptop job's supervisor.
#[derive(Debug)]
pub struct Supervise<'a, C, R> {
    /// The laptop's state.
    pub store: &'a Store,
    /// The `java` binary.
    pub java: PathBuf,
    /// The pinned jars, TLC's first.
    pub jars: Vec<PathBuf>,
    /// The tlx binary.
    pub tlx: PathBuf,
    /// The laptop's engine versions, for a move.
    pub versions: crate::engine::Versions,
    /// The slot, resources and cap.
    pub settings: Settings,
    /// The awake-time clock.
    pub clock: C,
    /// The runner, for a move.
    pub remote: R,
    /// A move's commit token.
    pub token: String,
}

/// How TLC's run ended.
struct Ran {
    /// Its exit code; `None` when killed.
    code: Option<i32>,
    /// Awake seconds it ran.
    secs: u64,
    /// Whether the cap stopped it.
    timed_out: bool,
}

impl<C: Clock, R: Remote> Supervise<'_, C, R> {
    /// Supervise run `run` of `id` until it ends.
    ///
    /// # Errors
    /// If the job's files cannot be read or written, or TLC not started. The
    /// job's lock frees as this process exits, so the orphan rule ends it.
    pub fn run(&mut self, id: &str, run: &str) -> Result<(), Error> {
        let dir = self.store.run_dir(id, run);
        let info = self
            .store
            .record(id)?
            .and_then(|r| r.info)
            .ok_or_else(|| std::io::Error::other(format!("{id}: no job record")))?;
        self.wait(id, &dir)?;
        let ran = self.tlc(id, &dir, &info)?;
        // TLC's own exit, even one that beat the cap's stop, has its verdict.
        let verdict = if ran.code.is_none() && ran.timed_out {
            None
        } else {
            let log = dir.join("tlc.log");
            let job = dir.join("job");
            Some(finished(
                info.engine,
                ran.code,
                &log,
                &job,
                "laptop",
                ran.secs,
                Vec::new(),
            )?)
        };
        // TLC is done: the slot and the disk are free for the next job.
        keep_curve(self.store, id, run)?;
        clean(self.store, id, run)?;
        match verdict {
            Some(result) => self.end(id, run, &result),
            None if info.vm_on_timeout => self.move_to_vm(id, run, ran.secs),
            None => self.end(id, run, &laptop_result(Outcome::Timeout, ran.secs)),
        }
    }

    /// Wait for the slot, then for free disk, writing where the run waits.
    fn wait(&self, id: &str, dir: &Path) -> Result<(), Error> {
        while let Some(phase) = self.look(id)? {
            let progress = Progress {
                phase,
                ..Progress::default()
            };
            write_json(&dir.join("progress.json"), &progress)?;
            self.clock.sleep(self.settings.poll_secs);
        }
        Ok(())
    }

    /// What run of `id` waits for, if anything: the slot, while as many jobs
    /// as it has are ahead (in ticket order), or the disk.
    fn look(&self, id: &str) -> std::io::Result<Option<Phase>> {
        let tickets = self.store.tickets()?;
        let mine = tickets.iter().position(|(_, t)| t == id);
        let mut ahead = 0;
        for (_, other) in tickets.iter().take(mine.unwrap_or(tickets.len())) {
            // A ticket whose lock is free is stale: its job is an orphan.
            if self.store.job_lock_held(other)? {
                ahead += 1;
            }
        }
        let free = self.store.free_bytes()?;
        Ok(if ahead >= self.settings.slots {
            Some(Phase::Slot {
                ahead: ahead + 1 - self.settings.slots,
            })
        } else if free < self.settings.disk_floor {
            Some(Phase::Disk {
                free_gb: free >> 30,
            })
        } else {
            None
        })
    }

    /// Run TLC on the extracted job, writing its progress, until it exits or
    /// the cap stops it.
    fn tlc(&self, id: &str, dir: &Path, info: &JobInfo) -> std::io::Result<Ran> {
        let job = dir.join("job");
        extract(
            &std::fs::read(self.store.tarball(id))?,
            &job,
            self.settings.limits,
        )?;
        let max = info.expected == Some(Expected::Max);
        let install = Install {
            java: &self.java,
            jars: &self.jars,
            tlx: &self.tlx,
        };
        let (program, args) = engine_command(
            install,
            self.settings.resources,
            info,
            max,
            &job,
            &dir.join("scratch"),
        );
        let log_path = dir.join("tlc.log");
        let log = std::fs::File::create(&log_path)?;
        let mut child = Command::new(&program)
            .args(&args)
            .current_dir(&job)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        // `--local` is uncapped unless `--timeout` caps it; else the laptop's
        // cap, or `--timeout` when that is shorter.
        let cap = if info.local {
            info.timeout_secs
        } else {
            Some(
                info.timeout_secs
                    .map_or(self.settings.cap_secs, |t| t.min(self.settings.cap_secs)),
            )
        };
        let start = self.clock.now();
        let mut tail = Tail::new(log_path.clone());
        let mut curve = Recorder::new(dir.join(curve::FILE))?;
        let mut queue = tlc::Pending::default();
        let mut progress = Progress {
            phase: Phase::Running,
            started_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs()),
            ..Progress::default()
        };
        let ran = loop {
            let status = child.try_wait()?;
            progress.elapsed_secs = self.clock.now().saturating_sub(start);
            for line in tail.lines()? {
                if let Some(c) = tlc::progress(&line).map(|c| queue.add(c)) {
                    progress.counters = Some(Counters {
                        generated: c.generated,
                        distinct: c.distinct,
                        queue: c.queue,
                    });
                }
                curve.line(progress.elapsed_secs, &line)?;
            }
            progress.left_secs = cap.map(|c| (c.saturating_sub(progress.elapsed_secs), c));
            if let Some(r) = Curve::read_in(dir)?.and_then(|c| c.rates(300.0)) {
                progress.generated_per_min = u64::try_from(whole(r.generated_per_min)).ok();
                progress.queue_growth_permille = Some(whole(r.queue_growth * 1000.0));
            }
            write_json(&dir.join("progress.json"), &progress)?;
            if let Some(status) = status {
                break Ran {
                    code: status.code(),
                    secs: progress.elapsed_secs,
                    timed_out: false,
                };
            }
            if cap.is_some_and(|c| progress.elapsed_secs >= c) {
                // The timed-out marker before anything else: a supervisor that
                // dies from here on still ends the job `timeout`.
                write_file(&dir.join("timed-out"), b"")?;
                child.kill()?;
                break Ran {
                    code: child.wait()?.code(),
                    secs: progress.elapsed_secs,
                    timed_out: true,
                };
            }
            self.clock.sleep(self.settings.poll_secs);
        };
        end_curve(&mut curve, &log_path, ran.secs)?;
        Ok(ran)
    }

    /// The record of `id`, under the placement lock, if it still is this
    /// laptop run's.
    fn own(&self, id: &str, run: &str) -> std::io::Result<Option<JobRecord>> {
        let record = self.store.record(id)?;
        Ok(record.filter(|r| r.state == State::Laptop && r.run.as_deref() == Some(run)))
    }

    /// End the job with `result`: `result.json`, then the record, the cache
    /// and the history.
    fn end(&self, id: &str, run: &str, result: &JobResult) -> Result<(), Error> {
        let lock = self.store.placement_lock()?;
        write_json(&self.store.run_dir(id, run).join("result.json"), result)?;
        if let Some(mut record) = self.own(id, run)? {
            record.elapsed_secs = result.duration_secs;
            snapshot::end(self.store, &lock, &mut record, result)?;
        }
        Ok(())
    }

    /// Move the job to the VM queue after its timeout (D7): under the
    /// placement lock, `moving`, then a commit of the same id, expected long,
    /// with every entry and the awake time so far, under the laptop run's
    /// name. An unknown outcome leaves it `moving`, for a later refresh.
    fn move_to_vm(&mut self, id: &str, run: &str, secs: u64) -> Result<(), Error> {
        let lock = self.store.placement_lock()?;
        let Some(mut record) = self.own(id, run)? else {
            return Ok(());
        };
        let mut info = record.info.clone().expect("a laptop job has its info");
        info.entries.clone_from(&record.entries);
        info.run = Some(run.to_owned());
        info.expected = Some(Expected::Long);
        info.source = None;
        info.local = false;
        info.laptop_secs = secs;
        record.state = State::Moving;
        record.token = Some(self.token.clone());
        record.elapsed_secs = secs;
        record.info = Some(info.clone());
        self.store.write_record(&lock, &mut record)?;
        let tarball = std::fs::read(self.store.tarball(id))?;
        let req = CommitRequest {
            versions: self.versions.clone(),
            token: self.token.clone(),
            pending: Vec::new(),
            jobs: vec![CommitJob {
                id: id.to_owned(),
                info,
                route: CommitRoute::Queue,
                tar_len: tarball.len() as u64,
            }],
            supersede: Vec::new(),
        };
        let wire = commit::encode(&req, &[tarball]).map_err(std::io::Error::other)?;
        let refused = |why: String| {
            laptop_result(
                Outcome::Error {
                    reason: Some(format!("its move to the VM failed: {why}")),
                },
                secs,
            )
        };
        let result = match self.remote.commit(&wire) {
            Ok(CommitReply::Applied { jobs, .. }) => match jobs.get(id) {
                Some(
                    JobAnswer::Started { run }
                    | JobAnswer::Queued { run }
                    | JobAnswer::Attached { run },
                ) => {
                    record.state = State::Vm;
                    record.token = None;
                    record.take_run(Some(run.clone()));
                    record.mirror = std::mem::take(&mut record.entries);
                    self.store.write_record(&lock, &mut record)?;
                    return Ok(());
                }
                Some(JobAnswer::Cached { result }) => result.clone(),
                Some(JobAnswer::Refused) | None => refused("the VM refused it".to_owned()),
            },
            Ok(CommitReply::Invalid { reason, .. }) => refused(reason),
            // Unknown: `moving`, lock-free once this exits, for the refresh.
            Ok(CommitReply::NotApplied) | Err(_) => return Ok(()),
        };
        write_json(&self.store.run_dir(id, run).join("result.json"), &result)?;
        snapshot::end(self.store, &lock, &mut record, &result)
    }
}

/// A rate or a share, rounded to a whole number.
#[expect(
    clippy::cast_possible_truncation,
    reason = "rates and permilles are far below 2^63"
)]
fn whole(v: f64) -> i64 {
    v.round() as i64
}

/// The awake-time clock: a monotonic clock that stops while the machine
/// sleeps (on macOS, Rust's `Instant`).
#[derive(Debug, Clone, Copy)]
pub struct AwakeClock(std::time::Instant);

impl AwakeClock {
    /// A clock from now.
    pub fn new() -> Self {
        Self(std::time::Instant::now())
    }
}

impl Default for AwakeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for AwakeClock {
    fn now(&self) -> u64 {
        self.0.elapsed().as_secs()
    }

    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}

#[cfg(test)]
#[path = "supervise_tests.rs"]
mod tests;

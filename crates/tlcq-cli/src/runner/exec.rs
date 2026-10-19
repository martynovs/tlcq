//! The VM supervisor, `tlcq exec <id> <run> <attempt>` (design D9, D10,
//! pinned-slots S5): the one process of an attempt's container. Its first act
//! is the started marker; it extracts the job, runs the engine with the slot's
//! resources, feeds its progress to the lane watch, keeps `result.json`
//! current, stops it for a pause at a checkpoint and keeps that checkpoint (as
//! it keeps a long run's when its engine dies), writes the stop markers, and
//! writes the verdict only when the engine exits on its own. Time its
//! container spent frozen counts for nothing.

use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::memory;
use super::state::Slot;
use super::store::{frozen_secs, AttemptResult, Checkpoint, Store};
use crate::curve::{self, Recorder};
use crate::engine::Engine;
use crate::files::{read_json, write_file, write_json};
use crate::job::{self, Limits};
use crate::lanes::{Act, Current, Settings, Watch};
use crate::outcome::{JobResult, LaneDecision, Outcome};
use crate::proto::JobInfo;
use crate::{must_fail, tlc};

/// The checkpoint interval both engines run with, in minutes: a pause lands
/// within about this.
pub const CHECKPOINT_MINUTES: u64 = 10;

/// How long a run runs before a death keeps its checkpoint, in seconds:
/// past this, rerunning it from the start costs more than the disk it holds.
pub const KEEP_AFTER_SECS: u64 = 3600;

/// The VM's resources and TLC's install (D9, D13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    /// The `java` binary.
    pub java: PathBuf,
    /// The pinned jars, TLC's first.
    pub jars: Vec<PathBuf>,
    /// The tlx binary.
    pub tlx: PathBuf,
    /// The share of a slot's memory for tlx's `-maxmem`, in percent.
    pub tlx_mem_percent: u64,
    /// The E slot's CPU list (`16-31`), as `--cpuset-cpus` takes it.
    pub e_cpus: String,
    /// The P slot's CPU list (`0-15`).
    pub p_cpus: String,
    /// The VM's memory, in MB.
    pub memory_mb: u64,
    /// Memory kept back, in MB.
    pub reserve_mb: u64,
    /// The share of a slot's memory for TLC's heap, in percent.
    pub heap_percent: u64,
    /// The share for off-heap fingerprints, in percent; the rest is the JVM's.
    pub direct_percent: u64,
    /// In an attempt's container, the memory cap the runner gave it, in MB,
    /// from what was free when it started ([`super::memory`]); it is then the
    /// slot's memory.
    pub slot_memory_mb: Option<u64>,
}

impl Machine {
    /// A slot's CPU list.
    pub fn cpus(&self, slot: Slot) -> &str {
        match slot {
            Slot::E => &self.e_cpus,
            Slot::P => &self.p_cpus,
        }
    }

    /// The most memory one attempt can get, in MB: the VM's less the
    /// reserve, the E slot's alone (whatever this attempt's own cap).
    pub fn most_mb(&self) -> u64 {
        self.memory_mb.saturating_sub(self.reserve_mb)
    }

    /// A slot's workers (its CPUs, at least one) and memory in MB: the
    /// attempt's own cap when it has one, else an equal share of the VM less
    /// the reserve among the open slots, all of it for the E slot `alone`
    /// (the most it can get).
    pub fn resources(&self, slot: Slot, alone: bool) -> (u64, u64) {
        let open = if alone { 1 } else { 2 };
        let share_mb = self.memory_mb.saturating_sub(self.reserve_mb) / open;
        (
            cpu_count(self.cpus(slot)).max(1),
            self.slot_memory_mb.unwrap_or(share_mb),
        )
    }

    /// The program and arguments that run a job extracted in `job` on its
    /// engine, in `slot`, its scratch in `scratch` (see [`engine_command`]);
    /// recovering `recover`'s checkpoint, in its directory, when given. TLC
    /// recovers with the checkpoint's workers and fingerprint function; tlx
    /// with the slot's workers, which may differ (tlx-engine E4).
    pub fn command(
        &self,
        slot: Slot,
        info: &JobInfo,
        max: bool,
        job: &Path,
        scratch: &Path,
        recover: Option<(Checkpoint, &Path)>,
    ) -> (PathBuf, Vec<String>) {
        let (workers, memory) = self.resources(slot, false);
        let tlc = info.engine == Engine::Tlc;
        let resources = Resources {
            workers: recover.filter(|_| tlc).map_or(workers, |(c, _)| c.workers),
            heap_mb: memory * self.heap_percent / 100,
            direct_mb: Some(memory * self.direct_percent / 100),
            tlx_mem_mb: memory * self.tlx_mem_percent / 100,
        };
        let install = Install {
            java: &self.java,
            jars: &self.jars,
            tlx: &self.tlx,
        };
        let (program, mut args) = engine_command(install, resources, info, max, job, scratch);
        if let Some((checkpoint, dir)) = recover {
            let mut options = vec!["-recover".to_owned(), dir.display().to_string()];
            if let Some(fp) = checkpoint.fp.filter(|_| tlc) {
                options.extend(["-fp".to_owned(), fp.to_string()]);
            }
            // Before the spec, which comes last for TLC.
            let spec = if tlc {
                args.len().saturating_sub(1)
            } else {
                args.len()
            };
            args.splice(spec..spec, options);
        }
        (program, args)
    }
}

/// The CPUs a list such as `0-15` or `0-3,8,10-11` names; a part it cannot
/// read counts for none.
pub fn cpu_count(list: &str) -> u64 {
    list.split(',')
        .filter_map(|part| {
            let part = part.trim();
            let (from, to) = part.split_once('-').unwrap_or((part, part));
            let (from, to): (u64, u64) = (from.parse().ok()?, to.parse().ok()?);
            (to >= from).then(|| to - from + 1)
        })
        .sum()
}

/// What one engine run may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resources {
    /// The engine's workers.
    pub workers: u64,
    /// TLC's heap, in MB.
    pub heap_mb: u64,
    /// TLC's off-heap memory for the fingerprints, in MB; `None` keeps them on the heap.
    pub direct_mb: Option<u64>,
    /// tlx's `-maxmem`, in MB.
    pub tlx_mem_mb: u64,
}

/// Where each engine is installed.
#[derive(Debug, Clone, Copy)]
pub struct Install<'a> {
    /// The `java` binary.
    pub java: &'a Path,
    /// The pinned jars, TLC's first.
    pub jars: &'a [PathBuf],
    /// The tlx binary.
    pub tlx: &'a Path,
}

/// The program and arguments that run a job extracted in `job` on `info`'s
/// engine: `java` with [`tlc_args`], or tlx with [`tlx_args`].
pub fn engine_command(
    install: Install<'_>,
    resources: Resources,
    info: &JobInfo,
    max: bool,
    job: &Path,
    scratch: &Path,
) -> (PathBuf, Vec<String>) {
    match info.engine {
        Engine::Tlc => (
            install.java.to_path_buf(),
            tlc_args(install.jars, resources, info, max, job, scratch),
        ),
        Engine::Tlx => (
            install.tlx.to_path_buf(),
            tlx_args(resources, info, job, scratch),
        ),
    }
}

/// tlx's arguments for a job extracted in `job`, its scratch in `scratch`
/// (tlx-engine E3): every path absolute, the allowlisted options. A max job
/// needs no `-lncheck`: tlx checks liveness after the search anyway. The
/// checkpoint interval is always given, [`CHECKPOINT_MINUTES`] so a pause lands
/// soon; without it, a run tlx guards with an unproven symmetry writes no
/// checkpoints at all. `-stats` adds tlx's report (phases, worker use, why no
/// automatic symmetry) to the log when tlx exits on its own.
pub fn tlx_args(resources: Resources, info: &JobInfo, job: &Path, scratch: &Path) -> Vec<String> {
    let model = job.join("model");
    let mut args = vec![
        "check".to_owned(),
        model
            .join(format!("{}.tla", info.spec))
            .display()
            .to_string(),
        "-config".to_owned(),
        model.join("job.cfg").display().to_string(),
        "-workers".to_owned(),
        resources.workers.to_string(),
        "-maxmem".to_owned(),
        format!("{}M", resources.tlx_mem_mb.max(1)),
        "-metadir".to_owned(),
        scratch.display().to_string(),
        "-checkpoint".to_owned(),
        CHECKPOINT_MINUTES.to_string(),
        // The report of where the time went, on stderr at the end: no cost seen.
        "-stats".to_owned(),
    ];
    for lib in lib_dirs(job) {
        args.extend(["-I".to_owned(), lib]);
    }
    args.extend(info.tlc_opts.iter().cloned());
    args
}

/// The `lib/<n>` directories the tarball holds, in EXTENDS order; one whose
/// directory gave no module is absent, so the numbers may skip.
fn lib_dirs(job: &Path) -> Vec<String> {
    let mut numbers: Vec<u64> = std::fs::read_dir(job.join("lib"))
        .map(|dir| {
            dir.flatten()
                .filter_map(|e| e.file_name().to_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    numbers.sort_unstable();
    numbers
        .iter()
        .map(|n| job.join("lib").join(n.to_string()).display().to_string())
        .collect()
}

/// TLC's arguments, with the jars `jars`, for a job extracted in `job`, its
/// scratch in `scratch`: every path absolute, the allowlisted options, a
/// checkpoint every [`CHECKPOINT_MINUTES`], `-lncheck final` for a max job.
pub fn tlc_args(
    jars: &[PathBuf],
    resources: Resources,
    info: &JobInfo,
    max: bool,
    job: &Path,
    scratch: &Path,
) -> Vec<String> {
    let mut args = vec![
        format!("-Xmx{}m", resources.heap_mb),
        "-XX:+UseParallelGC".to_owned(),
    ];
    if let Some(direct) = resources.direct_mb {
        args.extend([
            format!("-XX:MaxDirectMemorySize={direct}m"),
            "-Dtlc2.tool.fp.FPSet.impl=tlc2.tool.fp.OffHeapDiskFPSet".to_owned(),
        ]);
    }
    let libs = lib_dirs(job);
    if !libs.is_empty() {
        args.push(format!("-DTLA-Library={}", libs.join(":")));
    }
    let classpath: Vec<String> = jars.iter().map(|j| j.display().to_string()).collect();
    args.extend(["-cp".to_owned(), classpath.join(":"), "tlc2.TLC".to_owned()]);
    args.extend([
        "-workers".to_owned(),
        resources.workers.to_string(),
        "-noGenerateSpecTE".to_owned(),
    ]);
    args.extend(["-metadir".to_owned(), scratch.display().to_string()]);
    args.extend(["-checkpoint".to_owned(), CHECKPOINT_MINUTES.to_string()]);
    if resources.direct_mb.is_some() {
        args.extend(["-fpmem".to_owned(), "1.0".to_owned()]);
    }
    if max {
        args.extend(["-lncheck".to_owned(), "final".to_owned()]);
    }
    args.extend(info.tlc_opts.iter().cloned());
    let model = job.join("model");
    args.extend([
        "-config".to_owned(),
        model.join("job.cfg").display().to_string(),
    ]);
    args.push(
        model
            .join(format!("{}.tla", info.spec))
            .display()
            .to_string(),
    );
    args
}

/// The supervisor's clock (D12): seconds, and a way to wait.
pub trait Clock {
    /// Seconds since some fixed moment.
    fn now(&self) -> u64;

    /// Wait `secs` seconds.
    fn sleep(&self, secs: u64);
}

/// One attempt's supervisor.
#[derive(Debug)]
pub struct Exec<'a, C> {
    /// The runner's state directory, on the shared volume.
    pub store: &'a Store,
    /// The VM and TLC.
    pub machine: &'a Machine,
    /// The lanes' probe and caps.
    pub settings: Settings,
    /// The tarball limits, as the commit checked them.
    pub limits: Limits,
    /// The clock.
    pub clock: C,
    /// Seconds between looks at TLC.
    pub poll_secs: u64,
}

impl<C: Clock> Exec<'_, C> {
    /// Supervise attempt `number` of run `run` of `id` until its engine exits.
    ///
    /// # Errors
    /// If the attempt's files cannot be read or written, or the engine not started.
    pub fn run(&self, id: &str, run: &str, number: u32) -> std::io::Result<()> {
        let dir = self.store.attempt_dir(id, run, number);
        write_file(&dir.join("started"), b"")?;
        let (info, slot) = self.attempt(id, run, number)?;
        let mut first: AttemptResult = read_json(&dir.join("result.json"))?.unwrap_or_default();
        let job = dir.join("job");
        extract(&std::fs::read(self.store.tarball(id))?, &job, self.limits)?;
        let checkpoint = self.store.checkpoint(id, run)?;
        let mut watch = Watch::new(
            self.settings,
            info.expected,
            info.timeout_secs,
            first.vm_secs,
            first.lanes.clone(),
        );
        let max = watch.lane == Current::Max;
        let tlc_dir = self.store.checkpoint_dir(id, run).join("tlc");
        let recover = checkpoint.map(|c| (c, tlc_dir.as_path()));
        let (program, args) = self.machine_for(id, run, recover.is_some()).command(
            slot,
            &info,
            max,
            &job,
            &dir.join("scratch"),
            recover,
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
        let start = self.clock.now();
        first.attempt = number;
        first.started_at = Some(start);
        first.log = Some("tlc.log".to_owned());
        let mut tail = Tail::new(log_path.clone());
        // tlx's counts after a recovery already include the checkpoint's.
        let mut curve = recorder(&dir, checkpoint.filter(|_| info.engine == Engine::Tlc))?;
        let mut log_state = LogState::recovering(checkpoint);
        let mut stopped = false;
        let mut marked = None;
        let status = loop {
            let finished = child.try_wait()?;
            let secs = self.running_secs(id, run, number, start)?;
            feed(&mut tail, secs, &mut watch, &mut curve, &mut log_state)?;
            if checkpoint.is_some() && log_state.just {
                self.note_checkpoint(id, run, log_state.generated)?;
            }
            if let Some(status) = finished {
                break status;
            }
            let stop = self.due(id, run, number, watch.tick(secs), log_state)?;
            marked = marked.or(self.stop(&mut child, &dir, &mut stopped, stop)?);
            write_json(&dir.join("result.json"), &so_far(&first, &watch, secs))?;
            self.clock.sleep(self.poll_secs);
        };
        let now = self.clock.now();
        let secs = self.running_secs(id, run, number, start)?;
        let summary = end_curve(&mut curve, &log_path, secs)?;
        let mut result = so_far(&first, &watch, secs);
        result.ended_at = Some(now);
        result.out_of_memory = summary.out_of_memory;
        result.signal = status.signal();
        // The engine's own exit, even one that beat a stop (which kills it,
        // leaving no exit code): its verdict is kept, and settling ranks it over
        // the markers.
        if let Some(code) = status.code() {
            let verdict = finished(
                info.engine,
                Some(code),
                &log_path,
                &job,
                slot.place(),
                result.duration_secs,
                result.lanes.clone(),
            )?;
            result.finished = Some(self.noted(verdict, &curve));
        }
        if keeps(marked, stopped, &result) {
            self.keep_checkpoint(id, run, number, slot, &dir, log_state.generated)?;
        }
        write_json(&dir.join("result.json"), &result)
    }

    /// The engine's verdict with the run's generated states, its earlier
    /// recoveries counted by `curve`, and any memory hint.
    fn noted(&self, mut verdict: JobResult, curve: &Recorder) -> JobResult {
        verdict.generated = verdict.generated.map(|g| curve.carried(g));
        if verdict.note.is_none() {
            verdict.note = self.memory_hint(&verdict);
        }
        verdict
    }

    /// The hint of a verdict the engine gave for lack of memory (job-memory
    /// M6): tlx's estimate as the request to resubmit with, or the memory
    /// this attempt had when the engine ran out of it, and the most one job
    /// gets.
    fn memory_hint(&self, verdict: &JobResult) -> Option<String> {
        let most = self.machine.most_mb();
        memory::needed_hint(&verdict.errors, self.machine.tlx_mem_percent, most).or_else(|| {
            let oom = matches!(&verdict.outcome, Outcome::Error { reason: Some(r) } if r.contains("out of memory"));
            oom.then(|| self.machine.slot_memory_mb.map(|m| memory::killed_hint(m, most)))
                .flatten()
        })
    }

    /// The machine an attempt runs on: a recovery's tlx is the binary kept
    /// beside the run's checkpoint, the only one that recovers it (the
    /// image's may be newer); one kept before binaries were runs the image's.
    fn machine_for(&self, id: &str, run: &str, recovering: bool) -> Machine {
        let kept = self.store.checkpoint_dir(id, run).join("tlx");
        let mut machine = self.machine.clone();
        if recovering && kept.is_file() {
            machine.tlx = kept;
        }
        machine
    }

    /// Stop the engine, once (`stopped`): for a cancel marker, should the
    /// runner's own stop of the container be lost; or for `stop`, once its
    /// marker is written. Returns the stop whose marker it wrote.
    fn stop(
        &self,
        child: &mut std::process::Child,
        dir: &Path,
        stopped: &mut bool,
        stop: Option<Stop>,
    ) -> std::io::Result<Option<Stop>> {
        if *stopped {
            return Ok(None);
        }
        if dir.join("cancel.json").exists() {
            child.kill()?;
            *stopped = true;
            return Ok(None);
        }
        let Some(stop) = stop else {
            return Ok(None);
        };
        if !self.mark(dir, stop)? {
            return Ok(None);
        }
        child.kill()?;
        *stopped = true;
        Ok(Some(stop))
    }

    /// What stops the engine now, if anything: its cap (`act`), or a pause
    /// landing by what the `log` has said.
    fn due(
        &self,
        id: &str,
        run: &str,
        number: u32,
        act: Act,
        log: LogState,
    ) -> std::io::Result<Option<Stop>> {
        let dir = self.store.attempt_dir(id, run, number);
        // Once the search has ended, for the page: a pause asked now never lands.
        if log.searched && !dir.join("searched").exists() {
            write_file(&dir.join("searched"), b"")?;
        }
        Ok(match act {
            Act::Timeout => Some(Stop::Timeout),
            Act::Run if dir.join("pause").exists() => {
                log.pause_stop(self.unpaused(id, run, number)?)
            }
            Act::Run => None,
        })
    }

    /// The attempt's seconds of running since `start`: its time frozen left
    /// out, from its `frozen.log`.
    fn running_secs(&self, id: &str, run: &str, number: u32, start: u64) -> std::io::Result<u64> {
        let now = self.clock.now();
        let log = self.store.frozen_log(id, run, number)?;
        Ok(now
            .saturating_sub(start)
            .saturating_sub(frozen_secs(&log, start, now)))
    }

    /// Whether the attempt was frozen and is unpaused for its pause: then it
    /// stops at once.
    fn unpaused(&self, id: &str, run: &str, number: u32) -> std::io::Result<bool> {
        let log = self.store.frozen_log(id, run, number)?;
        Ok(log
            .lines()
            .last()
            .is_some_and(|l| l.split_whitespace().nth(1) == Some("unpause")))
    }

    /// The attempt's job description and slot, from the runner's state.
    fn attempt(&self, id: &str, run: &str, number: u32) -> std::io::Result<(JobInfo, Slot)> {
        let lock = self.store.lock()?;
        let state = self.store.load(&lock)?;
        let found = state
            .runs
            .get(id)
            .and_then(|runs| runs.iter().find(|r| r.name == run))
            .and_then(|r| {
                let a = r.attempts.iter().find(|a| a.number == number)?;
                Some((r.info.clone(), a.slot))
            });
        found.ok_or_else(|| std::io::Error::other(format!("{id} {run} {number}: no such attempt")))
    }

    /// A recovered run's engine checkpoints in place, over the one it
    /// recovered: record the run's generated states at the newest, so a
    /// resume after this attempt dies carries them on. Which attempt kept the
    /// checkpoint stays as it was.
    fn note_checkpoint(&self, id: &str, run: &str, generated: Option<u64>) -> std::io::Result<()> {
        let Some(kept) = self.store.checkpoint(id, run)? else {
            return Ok(());
        };
        write_json(
            &self.store.checkpoint_dir(id, run).join("checkpoint.json"),
            &Checkpoint { generated, ..kept },
        )
    }

    /// Keep this attempt's checkpoint, with the slot's workers and the
    /// image's tlx ([`keep_checkpoint`]).
    fn keep_checkpoint(
        &self,
        id: &str,
        run: &str,
        number: u32,
        slot: Slot,
        dir: &Path,
        generated: Option<u64>,
    ) -> std::io::Result<()> {
        let workers = self.machine.resources(slot, false).0;
        let tlx = Some(self.machine.tlx.as_path());
        keep_checkpoint(self.store, (id, run, number), dir, workers, tlx, generated)
    }

    /// Write `stop`'s marker, under the admission lock and only if no cancel
    /// marker is there, nor for a pause one with its request gone. Returns
    /// whether a marker was written: then the engine is to be stopped.
    fn mark(&self, dir: &Path, stop: Stop) -> std::io::Result<bool> {
        let _lock = self.store.lock()?;
        if dir.join("cancel.json").exists() {
            return Ok(false);
        }
        // A pause a human withdrew since (keep running) stops nothing.
        if stop != Stop::Timeout && !dir.join("pause").exists() {
            return Ok(false);
        }
        let name = match stop {
            Stop::Timeout => "timed-out",
            Stop::Paused => "paused",
            Stop::NoCheckpoint => "no-checkpoint",
        };
        write_file(&dir.join(name), b"")?;
        Ok(true)
    }
}

/// Why the supervisor stops its engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// The cap passed.
    Timeout,
    /// A pause, its checkpoint completed.
    Paused,
    /// A pause of a frozen job that never completed a checkpoint.
    NoCheckpoint,
}

/// What the engine's log has said so far of checkpoints and the search.
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per thing the log says"
)]
#[derive(Debug, Clone, Copy, Default)]
struct LogState {
    /// A checkpoint is being written.
    writing: bool,
    /// A checkpoint was completed, or one was recovered: the run has one to keep.
    completed: bool,
    /// The search has ended: its liveness check runs, no checkpoint comes.
    searched: bool,
    /// The last lines fed completed a checkpoint.
    just: bool,
    /// The run's generated states at the last completed checkpoint (the
    /// recovered one's, until this attempt completes its own).
    generated: Option<u64>,
    /// The queue of the progress lines as TLC counts it, for the lane watch.
    queue: tlc::Pending,
}

impl LogState {
    /// What the log has said before the engine starts: a recovered
    /// `checkpoint` is one the run can keep.
    fn recovering(checkpoint: Option<Checkpoint>) -> Self {
        Self {
            completed: checkpoint.is_some(),
            generated: checkpoint.and_then(|c| c.generated),
            ..Self::default()
        }
    }

    /// Read one line; `generated` is the run's count of generated states so far.
    fn line(&mut self, line: &str, generated: Option<u64>) {
        if tlc::checkpointing(line) {
            self.writing = true;
        }
        if tlc::checkpointed(line) {
            (self.writing, self.completed, self.just) = (false, true, true);
            // Before its first progress line, the recovered checkpoint's count holds.
            self.generated = generated.or(self.generated);
        }
        if tlc::final_liveness(line) {
            self.searched = true;
        }
    }

    /// Where a pause stands now: a running job stops just after a checkpoint
    /// completed (among the lines just fed), and not at all once its search has
    /// ended (none comes); one `unpaused` from frozen holds no slot, so it stops
    /// at once, once a checkpoint being written is done, with its last
    /// checkpoint or with none.
    fn pause_stop(self, unpaused: bool) -> Option<Stop> {
        if !unpaused {
            return (self.just && !self.searched).then_some(Stop::Paused);
        }
        if self.writing {
            None
        } else if self.completed {
            Some(Stop::Paused)
        } else {
            Some(Stop::NoCheckpoint)
        }
    }
}

/// The result of a run on `engine` that exited with `code`, read from its log
/// and the configuration of the job extracted in `job`; it ran at `place` for
/// `duration_secs`, with the lane decisions `lanes`.
pub(crate) fn finished(
    engine: Engine,
    code: Option<i32>,
    log: &Path,
    job: &Path,
    place: &str,
    duration_secs: u64,
    lanes: Vec<LaneDecision>,
) -> std::io::Result<JobResult> {
    // The job says which engine ran: tlx's early exits (a refused option or
    // checkpoint) come before the version line its log would start with.
    let summary = tlc::Summary {
        tlx: engine == Engine::Tlx,
        ..tlc::summary(&std::fs::read_to_string(log)?)
    };
    let cfg = std::fs::read_to_string(job.join("model").join("job.cfg"))?;
    let expected = must_fail::expectation(&cfg);
    let outcome = tlc::verdict(code, expected.as_ref(), &summary);
    let note =
        tlc::note(&outcome, expected.as_ref(), &summary).or_else(|| tlc::refusal(code, &summary));
    let counts = summary.counts;
    Ok(JobResult {
        outcome,
        exit_status: code,
        distinct: counts.map(|c| c.distinct),
        generated: counts.map(|c| c.generated),
        depth: counts.map(|c| c.depth),
        place: place.to_owned(),
        duration_secs,
        lanes,
        errors: summary.errors,
        note,
    })
}

/// Feed the engine's new log lines, seen `secs` into the attempt, to the lane
/// watch, the curve and what the log has said (`log`).
fn feed(
    tail: &mut Tail,
    secs: u64,
    watch: &mut Watch,
    curve: &mut Recorder,
    log: &mut LogState,
) -> std::io::Result<()> {
    log.just = false;
    for line in tail.lines()? {
        if let Some(counts) = tlc::progress(&line) {
            watch.progress(secs, log.queue.add(counts));
        }
        curve.line(secs, &line)?;
        log.line(&line, curve.generated());
    }
    Ok(())
}

/// The attempt's curve recorder in `dir`, carrying the run's count of
/// generated states on from the `checkpoint` it recovers, if any.
fn recorder(dir: &Path, checkpoint: Option<Checkpoint>) -> std::io::Result<Recorder> {
    let mut curve = Recorder::new(dir.join(curve::FILE))?;
    if let Some(generated) = checkpoint.and_then(|c| c.generated) {
        curve.recovering(generated);
    }
    Ok(curve)
}

/// Whether an attempt that ended as `result` has it keeps its checkpoint: a
/// pause's (its `marked` stop), or a long run's, an hour or more in, stopped
/// at its cap or whose engine died on its own (no stop, not `stopped` for a
/// cancel) with no verdict or an error, for the agent to resume, with more
/// memory or a longer cap say, or drop.
fn keeps(marked: Option<Stop>, stopped: bool, result: &AttemptResult) -> bool {
    let long = result.duration_secs >= KEEP_AFTER_SECS;
    let died = marked.is_none()
        && !stopped
        && result
            .finished
            .as_ref()
            .is_none_or(|f| matches!(f.outcome, Outcome::Error { .. }));
    marked == Some(Stop::Paused) || (long && (died || marked == Some(Stop::Timeout)))
}

/// Keep the checkpoint the engine last completed in attempt `number`'s
/// directory `dir`, for the run's resume to recover, taken at the run's
/// `generated` states, and record that the attempt kept it. One recovered is
/// already in place, the engine checkpointing into it; else it moves out of
/// the scratch, which settling deletes, tlx's with a copy of `tlx`, the binary
/// that wrote it and the only one that recovers it (none: a resume runs the
/// image's). TLC's recovers with its `workers`.
///
/// # Errors
/// If the checkpoint cannot be moved or its record written.
pub fn keep_checkpoint(
    store: &Store,
    (id, run, number): (&str, &str, u32),
    dir: &Path,
    workers: u64,
    tlx: Option<&Path>,
    generated: Option<u64>,
) -> std::io::Result<()> {
    let at = store.checkpoint_dir(id, run);
    let kept = if let Some(kept) = store.checkpoint(id, run)? {
        kept
    } else {
        let Some((written, engine)) = checkpoint_in(&dir.join("scratch"))? else {
            return Ok(());
        };
        std::fs::create_dir_all(&at)?;
        std::fs::rename(written, at.join("tlc"))?;
        // tlx always prints `fp 0`: only TLC's log names its function.
        let fp = match engine {
            Engine::Tlc => tlc::fingerprint_fn(&std::fs::read_to_string(dir.join("tlc.log"))?),
            Engine::Tlx => {
                if let Some(tlx) = tlx {
                    std::fs::copy(tlx, at.join("tlx"))?;
                }
                None
            }
        };
        Checkpoint {
            workers,
            fp,
            generated: None,
            attempt: None,
        }
    };
    write_json(
        &at.join("checkpoint.json"),
        &Checkpoint {
            generated,
            attempt: Some(number),
            ..kept
        },
    )
}

/// The checkpoint in the metadir `scratch`, and the engine whose layout it
/// has: tlx's is the metadir itself, holding `checkpoint/MANIFEST`; TLC's the
/// run directory under it holding `queue.chkpt`.
fn checkpoint_in(scratch: &Path) -> std::io::Result<Option<(PathBuf, Engine)>> {
    if scratch.join("checkpoint/MANIFEST").is_file() {
        return Ok(Some((scratch.to_path_buf(), Engine::Tlx)));
    }
    let Ok(entries) = std::fs::read_dir(scratch) else {
        return Ok(None);
    };
    for entry in entries {
        let path = entry?.path();
        if path.join("queue.chkpt").is_file() {
            return Ok(Some((path, Engine::Tlc)));
        }
    }
    Ok(None)
}

/// End the curve `secs` into the attempt, with the final counts of the log at
/// `log`. Returns what the log says of the run.
pub(crate) fn end_curve(
    curve: &mut Recorder,
    log: &Path,
    secs: u64,
) -> std::io::Result<tlc::Summary> {
    let summary = tlc::summary(&std::fs::read_to_string(log)?);
    curve.end(secs, summary.counts)?;
    Ok(summary)
}

/// `result.json` while the attempt runs: the run's duration and VM time so far,
/// and its lane decisions.
fn so_far(first: &AttemptResult, watch: &Watch, secs: u64) -> AttemptResult {
    AttemptResult {
        finished: None,
        duration_secs: first.duration_secs + secs,
        vm_secs: first.vm_secs + secs,
        lanes: watch.decisions.clone(),
        left_secs: Some(watch.left(secs)),
        cap_secs: Some(watch.cap_secs()),
        attempt: first.attempt,
        started_at: first.started_at,
        log: first.log.clone(),
        ..AttemptResult::default()
    }
}

/// Extract a job's tarball into `dir`, checked as the commit checked it,
/// within `limits`.
///
/// # Errors
/// If the tarball fails its checks or a file cannot be written.
pub fn extract(tarball: &[u8], dir: &Path, limits: Limits) -> std::io::Result<()> {
    let contents = crate::commit::read_tarball(tarball, limits).map_err(std::io::Error::other)?;
    for f in contents.files() {
        let path = dir.join(&f.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_file(&path, &f.bytes)?;
    }
    if !contents.files().iter().any(|f| f.path == job::CFG) {
        return Err(std::io::Error::other("the job has no configuration"));
    }
    Ok(())
}

/// The new complete lines of a growing log.
#[derive(Debug)]
pub(crate) struct Tail {
    path: PathBuf,
    offset: u64,
    partial: String,
}

impl Tail {
    /// A tail of `path` from its start.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            partial: String::new(),
        }
    }

    /// The lines completed since the last call.
    pub(crate) fn lines(&mut self) -> std::io::Result<Vec<String>> {
        let mut file = std::fs::File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        self.offset += file.read_to_end(&mut bytes)? as u64;
        self.partial.push_str(&String::from_utf8_lossy(&bytes));
        let mut lines: Vec<String> = self.partial.split('\n').map(str::to_owned).collect();
        self.partial = lines.pop().unwrap_or_default();
        Ok(lines)
    }
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;

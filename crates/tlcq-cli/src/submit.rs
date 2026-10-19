//! Placing a command's jobs (design D6), under the placement lock: the
//! snapshot, the plan, the writes before the commit, the commit, and the
//! records step. The input checks ran before ([`crate::batch`]).
//!
//! Before the commit, laptop supersedes are applied (an attach's entry in the
//! same record write) and every job about to be sent is written ahead as `sent`
//! with the commit's token, so a killed command always leaves a record of what
//! the VM may hold. After it, the answers turn `sent` records into `vm`, ended
//! or laptop runs; an `invalid` answer undoes the write-ahead; an unknown outcome
//! keeps it for a later lookup and starts nothing on the laptop.

use std::collections::BTreeMap;

use crate::batch::Job;
use crate::commit::{
    self, CommitJob, CommitReply, CommitRequest, CommitRoute, JobAnswer, PendingFlush,
    SupersedeOrder,
};
use crate::entry::Entry;
use crate::history::{Expected, History};
use crate::outcome::JobResult;
use crate::plan::{self, Kind, Lane, Plan, Planned, Route};
use crate::proto::{Flags, JobInfo, VmOlder};
use crate::remote::Remote;
use crate::runner::store::CancelMarker;
use crate::snapshot::{self, Error};
use crate::store::{Follow, Followed, JobRecord, PlacementLock, RunRef, State, Store};

/// The laptop's runs (D7): a seam, so placement is tested without supervisors.
pub trait Supervisors {
    /// Start a laptop run: take the job lock, write the ticket, then `record`
    /// (state `laptop`) and the tarball, then start the supervisor holding the lock.
    ///
    /// # Errors
    /// If a write fails or the supervisor does not start.
    fn launch(
        &mut self,
        store: &Store,
        lock: &PlacementLock,
        record: &mut JobRecord,
        tarball: &[u8],
    ) -> std::io::Result<()>;

    /// Cancel a laptop job by `cancel`'s procedure, under the placement lock:
    /// the marker, the supervisor stopped, the orphan rule applied.
    ///
    /// # Errors
    /// If a write fails.
    fn cancel(
        &mut self,
        store: &Store,
        lock: &PlacementLock,
        record: &mut JobRecord,
        marker: &CancelMarker,
    ) -> std::io::Result<()>;
}

/// A command's placement inputs beside its jobs.
#[derive(Debug, Clone)]
pub struct Submit<'a> {
    /// The command, for the plan.
    pub cmd: plan::Command<'a>,
    /// The laptop's jar version.
    pub versions: crate::engine::Versions,
    /// `--vm-on-timeout`.
    pub vm_on_timeout: bool,
    /// The laptop's slot count.
    pub slots: usize,
    /// The commit's token, random.
    pub token: String,
}

/// Where a job ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placed {
    /// A cache hit.
    Cached(JobResult),
    /// A new laptop run.
    Laptop(String),
    /// An entry on a laptop run.
    LaptopAttach(String),
    /// The VM's answer.
    Vm(JobAnswer),
    /// A pending entry, while the VM is unreachable.
    Pending(RunRef),
    /// Not started: the commit's outcome is unknown.
    NotStarted,
}

/// What a command did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The plan it carried out (or its refusals).
    pub plan: Plan,
    /// Each job's placement, by id.
    pub placed: BTreeMap<String, Placed>,
    /// Pending entries the commit dropped: their run had ended or was unknown.
    pub dropped: Vec<PendingFlush>,
    /// The `invalid` answer: the job and why.
    pub invalid: Option<(String, String)>,
    /// The exit code: 0, 2 refused or invalid, 3 the commit's outcome unknown.
    pub exit: u8,
    /// The older versions the VM reported, for the refusals' details.
    pub older: Vec<VmOlder>,
    /// Each committed job as the VM had it after the commit: its slot, or its
    /// place in the queue.
    pub views: BTreeMap<String, crate::proto::VmJob>,
}

/// A warning for each attach whose flags differ from those the job already
/// runs with, its first submitter's, which it keeps (D6).
fn flag_warnings(s: &Submit<'_>, plan: &Plan, snap: &snapshot::Snapshot) -> Vec<String> {
    let mut warnings = Vec::new();
    for p in &plan.jobs {
        let first = match p.kind {
            Kind::AttachVm => snap
                .vm
                .as_ref()
                .and_then(|v| v.jobs.get(&p.id))
                .map(|j| j.flags.clone()),
            Kind::AttachLaptop => snap
                .laptop
                .records
                .get(&p.id)
                .and_then(|r| r.info.as_ref())
                .map(JobInfo::flags),
            _ => None,
        };
        let Some(job) = s.cmd.jobs.iter().find(|j| j.id == p.id) else {
            continue;
        };
        let timeout_secs = job
            .names
            .iter()
            .filter_map(|n| n.options.timeout)
            .max()
            .map(|d| d.as_secs());
        let mine = Flags {
            timeout_secs,
            vm_on_timeout: s.vm_on_timeout,
            memory_mb: job.memory_mb(),
        };
        if let Some(first) = first.filter(|f| *f != mine) {
            warnings.push(format!(
                "{}: attached to a run that keeps its first submitter's flags (timeout {}, vm-on-timeout {}, memory {}), not these (timeout {}, vm-on-timeout {}, memory {})",
                crate::report::short(&p.id),
                secs(first.timeout_secs),
                first.vm_on_timeout,
                mb(first.memory_mb),
                secs(mine.timeout_secs),
                mine.vm_on_timeout,
                mb(mine.memory_mb),
            ));
        }
    }
    warnings
}

/// A timeout as a warning names it.
fn secs(timeout: Option<u64>) -> String {
    timeout.map_or_else(|| "none".to_owned(), |s| format!("{s}s"))
}

/// A memory request as a warning names it.
fn mb(memory: Option<u64>) -> String {
    memory.map_or_else(|| "none".to_owned(), |m| format!("{m}M"))
}

/// Place `s`'s jobs.
///
/// # Errors
/// If the laptop's state cannot be read or written, or the preflight connects
/// and fails (exit 3, nothing written).
pub fn submit(
    store: &Store,
    remote: &mut impl Remote,
    sups: &mut impl Supervisors,
    s: &Submit<'_>,
) -> Result<Report, Error> {
    let lock = store.placement_lock()?;
    let ids: Vec<String> = s.cmd.jobs.iter().map(|j| j.id.clone()).collect();
    let keys = s
        .cmd
        .jobs
        .iter()
        .flat_map(|j| s.cmd.entries(j))
        .map(|e| e.key)
        .collect();
    let snap = snapshot::take(store, &lock, remote, &s.versions, &ids, keys, s.slots)?;
    // After the refresh, which records every run it learns has ended (D6).
    let history = History::load(&store.history())?;
    let mut plan = plan::plan(&s.cmd, &snap.laptop, snap.vm.as_ref(), &history);
    plan.warnings.extend(flag_warnings(s, &plan, &snap));
    let mut run = Placement {
        store,
        lock: &lock,
        s,
        records: snap.laptop.records,
        reachable: snap.vm.is_some(),
        follows: store.follows()?,
        report: Report {
            plan: plan.clone(),
            older: snap
                .vm
                .as_ref()
                .map(|v| v.older.clone())
                .unwrap_or_default(),
            ..Report::default()
        },
    };
    if !plan.refusals.is_empty() {
        run.report.exit = 2;
        return Ok(run.report);
    }
    run.supersede_on_laptop(sups)?;
    run.cache_hits()?;
    let Some(answer) = run.commit(remote)? else {
        run.on_laptop(sups, true)?;
        return Ok(run.report);
    };
    match answer {
        Answer::Applied(jobs, dropped) => {
            run.records_step(sups, &jobs)?;
            run.superseded_on_vm()?;
            run.report.dropped = dropped;
            run.on_laptop(sups, true)?;
        }
        Answer::Invalid(written, id, reason) => {
            run.undo(written)?;
            run.report.invalid = Some((id, reason));
            run.report.exit = 2;
        }
        Answer::Unknown => {
            run.on_laptop(sups, false)?;
            run.report.exit = 3;
        }
    }
    Ok(run.report)
}

/// What the commit came to.
enum Answer {
    /// Applied: each job's answer, and the pending entries dropped.
    Applied(BTreeMap<String, JobAnswer>, Vec<PendingFlush>),
    /// Refused as `invalid`: the write-ahead (each record as it was before, and
    /// the follows written) to undo, the job and why.
    Invalid(WriteAhead, String, String),
    /// Unknown: no reply, or a token a lookup tombstoned.
    Unknown,
}

/// What the write-ahead wrote: each record as it was before (`None`: created),
/// and the follows.
#[derive(Debug, Default)]
struct WriteAhead {
    records: Vec<(String, Option<JobRecord>)>,
    follows: Vec<u64>,
}

/// One command's placement in progress.
struct Placement<'a> {
    store: &'a Store,
    lock: &'a PlacementLock,
    s: &'a Submit<'a>,
    records: BTreeMap<String, JobRecord>,
    reachable: bool,
    follows: Vec<Follow>,
    report: Report,
}

impl Placement<'_> {
    /// The job `id` of the command.
    fn job(&self, id: &str) -> &Job {
        self.s
            .cmd
            .jobs
            .iter()
            .find(|j| j.id == id)
            .expect("the plan holds only the command's jobs")
    }

    /// The planned jobs with these routes.
    fn routed(&self, routes: &[Route]) -> Vec<Planned> {
        let jobs = self.report.plan.jobs.iter();
        jobs.filter(|p| routes.contains(&p.route))
            .cloned()
            .collect()
    }

    /// Supersede the laptop's older versions, and drop this worktree's pending
    /// entries on the VM's, before the commit. A laptop job this command also
    /// attaches to gets the attach's entry in the same write, and is never cancelled.
    fn supersede_on_laptop(&mut self, sups: &mut impl Supervisors) -> std::io::Result<()> {
        for sup in self.report.plan.supersede.clone() {
            let attach = sup.attached.then(|| self.s.cmd.entries(self.job(&sup.old)));
            let Some(old) = self.records.get_mut(&sup.old) else {
                continue;
            };
            if !sup.on_laptop {
                old.pending.retain(|p| p.entry.key != sup.key);
                self.store.write_record(self.lock, old)?;
                continue;
            }
            old.entries.retain(|e| e.key != sup.key);
            add_entries(&mut old.entries, &attach.unwrap_or_default());
            if old.entries.is_empty() {
                let marker = CancelMarker {
                    by: self.s.cmd.label.to_string(),
                    replaced_by: Some(sup.by.clone()),
                };
                sups.cancel(self.store, self.lock, old, &marker)?;
            } else {
                self.store.write_record(self.lock, old)?;
            }
        }
        Ok(())
    }

    /// Cache hits: an ended record carrying the agent's entries, and its follows.
    fn cache_hits(&mut self) -> Result<(), Error> {
        for p in self.report.plan.jobs.clone() {
            let Kind::CacheHit(result) = &p.kind else {
                continue;
            };
            let entries = self.s.cmd.entries(self.job(&p.id));
            let record = self
                .records
                .entry(p.id.clone())
                .or_insert_with(|| JobRecord::new(&p.id, State::Ended));
            if record.state == State::Ended {
                add_entries(&mut record.entries, &entries);
                record.result = Some(result.clone());
                self.store.write_record(self.lock, record)?;
            }
            // A hit on the VM's cache is the laptop's too, for offline (D6).
            self.store.cache(&p.id, result)?;
            self.follow(&p.id, &entries, &Followed::Result(result.clone()))?;
            self.report
                .placed
                .insert(p.id.clone(), Placed::Cached(result.clone()));
        }
        Ok(())
    }

    /// Write ahead and commit, if the VM is reachable and there is anything to
    /// send. `None`: no commit.
    fn commit(&mut self, remote: &mut impl Remote) -> Result<Option<Answer>, Error> {
        let sends = self.routed(&[
            Route::VmQueue,
            Route::VmNowOrRefuse,
            Route::VmNowOrQueue,
            Route::VmAttach,
        ]);
        let supersede: Vec<SupersedeOrder> = self
            .report
            .plan
            .supersede
            .iter()
            .filter(|s| !s.on_laptop)
            .map(|s| SupersedeOrder {
                old: s.old.clone(),
                key: s.key.clone(),
                by: s.by.clone(),
                label: self.s.cmd.label.to_string(),
                attached: s.attached,
            })
            .collect();
        if !self.reachable || (sends.is_empty() && supersede.is_empty()) {
            return Ok(None);
        }
        let (written, jobs, tarballs) = self.write_ahead(&sends)?;
        let pending = self
            .records
            .values()
            .flat_map(|r| {
                r.pending.iter().map(|p| PendingFlush {
                    id: r.id.clone(),
                    run: p.run.clone(),
                    entry: p.entry.clone(),
                })
            })
            .collect();
        let req = CommitRequest {
            versions: self.s.versions.clone(),
            token: self.s.token.clone(),
            pending,
            jobs,
            supersede,
        };
        let wire = commit::encode(&req, &tarballs).map_err(std::io::Error::other)?;
        Ok(Some(match remote.commit(&wire) {
            Ok(CommitReply::Applied {
                jobs,
                dropped,
                views,
            }) => {
                self.clear_pending(&dropped)?;
                self.report.views = views;
                Answer::Applied(jobs, dropped)
            }
            Ok(CommitReply::Invalid { id, reason }) => Answer::Invalid(written, id, reason),
            Ok(CommitReply::NotApplied) | Err(_) => Answer::Unknown,
        }))
    }

    /// Write every job about to be sent as `sent`, with the commit's token and
    /// this command's entries in its mirror, and its follows by token.
    fn write_ahead(
        &mut self,
        sends: &[Planned],
    ) -> std::io::Result<(WriteAhead, Vec<CommitJob>, Vec<Vec<u8>>)> {
        let mut written = WriteAhead::default();
        let (mut jobs, mut tarballs) = (Vec::new(), Vec::new());
        for p in sends {
            let job = self.job(&p.id).clone();
            let entries = self.s.cmd.entries(&job);
            let info = self.info(&job, p);
            let before = self.records.get(&p.id).cloned();
            let record = self
                .records
                .entry(p.id.clone())
                .or_insert_with(|| JobRecord::new(&p.id, State::Sent));
            if p.route != Route::VmAttach {
                // A new run: nothing of an earlier run of the id (its entries
                // would read as an older version still running) carries over,
                // but a VM run not yet ended (still stopping) is kept, for its
                // end to reach the history.
                if record.state == State::Vm {
                    record.take_run(None);
                }
                record.run = None;
                record.info = Some(info.clone());
                record.mirror.clear();
                record.entries.clear();
                record.result = None;
            }
            record.state = State::Sent;
            record.token = Some(self.s.token.clone());
            add_entries(&mut record.mirror, &entries);
            self.store.write_record(self.lock, record)?;
            written.records.push((p.id.clone(), before));
            let what = Followed::Run(RunRef::Token(self.s.token.clone()));
            written.follows.extend(self.follow(&p.id, &entries, &what)?);
            let tarball = job.contents.tarball()?;
            let route = match p.route {
                Route::VmNowOrRefuse => CommitRoute::NowOrRefuse,
                Route::VmNowOrQueue => CommitRoute::NowOrQueue,
                _ => CommitRoute::Queue,
            };
            jobs.push(CommitJob {
                id: p.id.clone(),
                info,
                route,
                tar_len: tarball.len() as u64,
            });
            tarballs.push(tarball);
        }
        Ok((written, jobs, tarballs))
    }

    /// The commit applied its supersedes: each VM older version lost this
    /// command's entry under the key, so the laptop's mirror of it does too
    /// (else, offline, it would read as an older version still running).
    fn superseded_on_vm(&mut self) -> std::io::Result<()> {
        for sup in self.report.plan.supersede.iter().filter(|s| !s.on_laptop) {
            if let Some(old) = self.records.get_mut(&sup.old) {
                old.mirror.retain(|e| e.key != sup.key);
                self.store.write_record(self.lock, old)?;
            }
        }
        Ok(())
    }

    /// The commit carried every pending entry: clear them, each flushed one
    /// (not `dropped`) into its job's mirror.
    fn clear_pending(&mut self, dropped: &[PendingFlush]) -> std::io::Result<()> {
        for r in self.records.values_mut().filter(|r| !r.pending.is_empty()) {
            // A flushed entry is now the VM's: the mirror shows it, so the
            // laptop, offline, still knows this worktree holds the job.
            for p in std::mem::take(&mut r.pending) {
                let lost = dropped
                    .iter()
                    .any(|d| d.id == r.id && d.run == p.run && d.entry == p.entry);
                if !lost {
                    add_entries(&mut r.mirror, std::slice::from_ref(&p.entry));
                }
            }
            self.store.write_record(self.lock, r)?;
        }
        Ok(())
    }

    /// The records step: each sent job's record and follows from its answer.
    fn records_step(
        &mut self,
        sups: &mut impl Supervisors,
        answers: &BTreeMap<String, JobAnswer>,
    ) -> Result<(), Error> {
        for (id, answer) in answers {
            let (what, placed) = match answer {
                JobAnswer::Started { run }
                | JobAnswer::Queued { run }
                | JobAnswer::Attached { run } => {
                    let record = self.records.get_mut(id).expect("written ahead");
                    record.state = State::Vm;
                    record.take_run(Some(run.clone()));
                    record.token = None;
                    self.store.write_record(self.lock, record)?;
                    (
                        Followed::Run(RunRef::Run(run.clone())),
                        Placed::Vm(answer.clone()),
                    )
                }
                JobAnswer::Cached { result } => {
                    let record = self.records.get_mut(id).expect("written ahead");
                    snapshot::end(self.store, self.lock, record, result)?;
                    (Followed::Result(result.clone()), Placed::Vm(answer.clone()))
                }
                JobAnswer::Refused => {
                    let run = self.launch(sups, id)?;
                    (Followed::Run(RunRef::Run(run.clone())), Placed::Laptop(run))
                }
            };
            self.resolve_follows(id, &what)?;
            self.report.placed.insert(id.clone(), placed);
        }
        Ok(())
    }

    /// The laptop's routes: new laptop runs (only when `start`), entries on
    /// laptop runs, and pending entries.
    fn on_laptop(&mut self, sups: &mut impl Supervisors, start: bool) -> Result<(), Error> {
        for p in self.routed(&[Route::Laptop, Route::LaptopAttach, Route::Pending]) {
            let entries = self.s.cmd.entries(self.job(&p.id));
            let (placed, what) = match (&p.route, &p.kind) {
                (Route::Laptop, _) if !start => {
                    self.report.placed.insert(p.id.clone(), Placed::NotStarted);
                    continue;
                }
                (Route::Laptop, _) => {
                    let run = self.launch(sups, &p.id)?;
                    (Placed::Laptop(run.clone()), Followed::Run(RunRef::Run(run)))
                }
                (Route::Pending, Kind::AttachOffline(run)) => {
                    let record = self
                        .records
                        .get_mut(&p.id)
                        .expect("an offline attach has a record");
                    for e in &entries {
                        record.pending.push(crate::store::Pending {
                            entry: e.clone(),
                            run: run.clone(),
                        });
                    }
                    self.store.write_record(self.lock, record)?;
                    (Placed::Pending(run.clone()), Followed::Run(run.clone()))
                }
                _ => {
                    let record = self
                        .records
                        .get_mut(&p.id)
                        .expect("a laptop attach has a record");
                    add_entries(&mut record.entries, &entries);
                    self.store.write_record(self.lock, record)?;
                    let run = record.run.clone().unwrap_or_default();
                    (
                        Placed::LaptopAttach(run.clone()),
                        Followed::Run(RunRef::Run(run)),
                    )
                }
            };
            self.follow(&p.id, &entries, &what)?;
            self.report.placed.insert(p.id.clone(), placed);
        }
        Ok(())
    }

    /// Start a new laptop run of `id`. Returns its name.
    fn launch(&mut self, sups: &mut impl Supervisors, id: &str) -> std::io::Result<String> {
        let job = self.job(id).clone();
        let planned = self
            .report
            .plan
            .jobs
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .expect("planned");
        let info = self.info(&job, &planned);
        let record = self
            .records
            .entry(id.to_owned())
            .or_insert_with(|| JobRecord::new(id, State::Laptop));
        record.laptop_runs += 1;
        let run = format!("l{}", record.laptop_runs);
        record.state = State::Laptop;
        record.run = Some(run.clone());
        record.token = None;
        record.entries.clone_from(&info.entries);
        record.mirror.clear();
        record.result = None;
        record.info = Some(info);
        sups.launch(self.store, self.lock, record, &job.contents.tarball()?)?;
        Ok(run)
    }

    /// Undo the write-ahead after an `invalid` answer: records restored or
    /// removed, follows removed.
    fn undo(&mut self, written: WriteAhead) -> std::io::Result<()> {
        for (id, before) in written.records {
            match before {
                Some(mut r) => self.store.write_record(self.lock, &mut r)?,
                None => self.store.remove_record(self.lock, &id)?,
            }
        }
        for number in written.follows {
            self.store.remove_follow(self.lock, number)?;
        }
        Ok(())
    }

    /// One follow per entry this command places on `id`, unless the agent
    /// already follows that very run under that key. Returns the numbers written.
    fn follow(
        &mut self,
        id: &str,
        entries: &[Entry],
        what: &Followed,
    ) -> std::io::Result<Vec<u64>> {
        let mut numbers = Vec::new();
        for e in entries {
            if latest_is(&self.follows, id, e, what) {
                continue;
            }
            let number = self.follows.iter().map(|f| f.number).max().unwrap_or(0) + 1;
            let follow = Follow {
                number,
                key: e.key.clone(),
                label: e.label.clone(),
                id: id.to_owned(),
                what: what.clone(),
            };
            self.store.write_follow(self.lock, &follow)?;
            self.follows.push(follow);
            numbers.push(number);
        }
        Ok(numbers)
    }

    /// Rewrite this command's follows of `id` by token to `what`; one that now
    /// matches the agent's latest follow under its key is removed instead.
    fn resolve_follows(&mut self, id: &str, what: &Followed) -> std::io::Result<()> {
        let token = Followed::Run(RunRef::Token(self.s.token.clone()));
        let mine: Vec<Follow> = self
            .follows
            .iter()
            .filter(|f| f.id == id && f.what == token)
            .cloned()
            .collect();
        for mut f in mine {
            self.follows.retain(|o| o.number != f.number);
            let entry = Entry {
                key: f.key.clone(),
                label: f.label.clone(),
            };
            if latest_is(&self.follows, id, &entry, what) {
                self.store.remove_follow(self.lock, f.number)?;
                continue;
            }
            f.what = what.clone();
            self.store.write_follow(self.lock, &f)?;
            self.follows.push(f);
        }
        Ok(())
    }

    /// A job's description, `job.json`.
    fn info(&self, job: &Job, planned: &Planned) -> JobInfo {
        let first = job.names.first().expect("a job has a name");
        JobInfo {
            entries: self.s.cmd.entries(job),
            run: None,
            spec: first.keys.spec.clone(),
            engine: job.engine,
            tlc_opts: first.options.tlc_opts.clone(),
            names: job.names.iter().map(|n| n.keys.clone()).collect(),
            expected: expected(planned.lane),
            source: planned.source,
            // A merged job keeps the longest cap any of its names asks for.
            timeout_secs: job
                .names
                .iter()
                .filter_map(|n| n.options.timeout)
                .max()
                .map(|d| d.as_secs()),
            local: self.s.cmd.local,
            vm_on_timeout: self.s.vm_on_timeout,
            // And the largest memory request.
            memory_mb: job.memory_mb(),
            laptop_secs: 0,
        }
    }
}

/// Whether the agent's latest follow under `e`'s key already follows run
/// `what` of `id`: then a placement adds none. Only the latest counts: a newer
/// follow of something else (a send that never arrived, a cancelled version)
/// must not hide this placement from `status`.
fn latest_is(follows: &[Follow], id: &str, e: &Entry, what: &Followed) -> bool {
    let latest = follows
        .iter()
        .filter(|f| f.key == e.key && f.label.same_agent(&e.label))
        .max_by_key(|f| f.number);
    matches!(what, Followed::Run(RunRef::Run(_)))
        && latest.is_some_and(|f| f.id == id && &f.what == what)
}

/// A lane as `job.json` gives it.
fn expected(lane: Lane) -> Option<Expected> {
    match lane {
        Lane::Short => Some(Expected::Short),
        Lane::Unknown => None,
        Lane::Long => Some(Expected::Long),
        Lane::Max => Some(Expected::Max),
    }
}

/// Add `new` to `entries`, each once.
fn add_entries(entries: &mut Vec<Entry>, new: &[Entry]) {
    for e in new {
        if !entries.contains(e) {
            entries.push(e.clone());
        }
    }
}

#[cfg(test)]
#[path = "submit_tests.rs"]
pub(crate) mod tests;

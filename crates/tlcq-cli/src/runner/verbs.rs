//! The VM verbs (design D6, D9, D10): each reads and checks its payload before
//! the admission lock, then under it settles gone attempts, applies, admits and
//! saves, and replies after releasing it.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Read, Write};

use super::admit::{Launch, Move};
use super::settle::Settled;
use super::state::{Attempt, Freeze, Phase, Run, Slot, State};
use super::store::{AdmissionLock, AttemptResult, CancelMarker, Store};
use crate::commit::{self, CommitReply, ReadError};
use crate::curve::Curve;
use crate::job::Limits;
use crate::proto::{
    AttemptCurve, CancelRequest, CommitAnswer, FetchFile, FetchReply, FetchRequest, PreflightReply,
    PreflightRequest, ResumeReply, ResumeRequest, VmJob,
};

/// The attempt containers (D12): podman on pve, a fake in tests.
pub trait Containers {
    /// The names of the attempt containers still running, a paused one
    /// included: it is frozen, not gone.
    ///
    /// # Errors
    /// If podman cannot be asked.
    fn running(&self) -> std::io::Result<BTreeSet<String>>;

    /// The names of the attempt containers paused now.
    ///
    /// # Errors
    /// If podman cannot be asked.
    fn paused(&self) -> std::io::Result<BTreeSet<String>>;

    /// The memory cap, in MB, a new attempt in `slot` would get now, `alone`
    /// with the P slot closed, the `frozen` containers' unused limits not
    /// promised; `None` if too little is free, so it waits ([`super::memory`]).
    fn memory(&self, slot: Slot, alone: bool, frozen: &BTreeSet<String>) -> Option<u64>;

    /// The most memory an attempt can get: in a slot with both slots open,
    /// and in the E slot alone.
    fn shares(&self) -> super::admit::Shares;

    /// The workers an attempt in `slot` runs with.
    fn workers(&self, slot: Slot) -> u64;

    /// The VM's free memory now, in MB, if it can be read.
    fn free_mb(&self) -> Option<u64>;

    /// The memory a new attempt could start with now, in MB, before its
    /// slot's share caps it: what is free less the reserve and the room the
    /// running attempts may still grow into ([`super::memory::cap_mb`]).
    fn room_mb(&self) -> Option<u64>;

    /// Pause a container: its processes stop, its memory kept.
    ///
    /// # Errors
    /// Why it was not paused.
    fn pause(&mut self, name: &str) -> Result<(), String>;

    /// Unpause a paused container.
    ///
    /// # Errors
    /// Why it was not unpaused.
    fn unpause(&mut self, name: &str) -> Result<(), String>;

    /// Move a container, paused or not, to `slot`'s CPUs.
    ///
    /// # Errors
    /// Why it was not moved.
    fn move_to(&mut self, name: &str, slot: Slot) -> Result<(), String>;

    /// Start an attempt's container, `memory_mb` its memory cap.
    ///
    /// # Errors
    /// Why it did not start.
    fn start(
        &mut self,
        name: &str,
        id: &str,
        run: &Run,
        attempt: &Attempt,
        memory_mb: u64,
    ) -> Result<(), String>;

    /// Ask an attempt's container to stop, without waiting for it (a paused
    /// one is killed as it is).
    fn stop(&mut self, name: &str);

    /// Remove a settled attempt's container.
    fn remove(&mut self, name: &str);

    /// Remove the images no container uses and no tag names (old builds'
    /// layers), best effort.
    fn prune_images(&mut self);
}

/// Why a request for `mb` is refused, past the `most` one job can get.
fn too_much(mb: u64, most: u64) -> String {
    format!("it asks for {mb} MB, more than the {most} MB the VM can give (its memory less the reserve)")
}

/// An attempt's container name: no two attempts share one.
pub fn container_name(id: &str, run: &str, attempt: u32) -> String {
    format!("tlcq-{id}-{run}-{attempt}")
}

/// The runner's side of every verb.
#[derive(Debug)]
pub struct Runner<C> {
    /// The state directory.
    pub store: Store,
    /// The attempt containers.
    pub containers: C,
    /// The VM's jar version.
    pub versions: crate::engine::Versions,
    /// The tarball limits.
    pub limits: Limits,
    /// How many attempts in a row may be undone before the run ends `error`.
    pub never_started: u32,
    /// The free bytes the state volume needs for admission to start a job.
    pub disk_floor: u64,
}

impl<C: Containers> Runner<C> {
    /// The preflight: read-only but for token tombstones. Gone attempts are
    /// reported as settling would, without settling them.
    ///
    /// # Errors
    /// If the state or podman cannot be read, or the state written.
    pub fn preflight(&mut self, req: &PreflightRequest) -> std::io::Result<PreflightReply> {
        let lock = self.store.lock()?;
        let mut state = self.store.load(&lock)?;
        let mut view = state.clone();
        self.settle_view(&mut view)?;
        let mut reply = view.preflight(req, &self.versions);
        let before = state.tokens.len();
        state.preflight(req, &self.versions);
        if state.tokens.len() != before {
            self.store.save(&lock, &state)?;
        }
        self.fill(&mut reply, &last_attempts(&view), &view);
        Ok(reply)
    }

    /// The commit: every job checked before the lock (a failure answers
    /// `invalid`, nothing applied); then under it settling, the apply, the
    /// stops of superseded attempts, one admission round and the answers.
    ///
    /// # Errors
    /// If the commit cannot be read, or the state or podman fails.
    pub fn commit(&mut self, input: impl BufRead) -> Result<CommitReply, ReadError> {
        let (req, tarballs) = commit::decode(input, self.limits.bytes)?;
        let most = self.containers.shares().whole_mb;
        for (job, tarball) in req.jobs.iter().zip(&tarballs) {
            let checked = commit::check_job(job, tarball, &self.versions, self.limits).and_then(
                |_| match job.info.memory_mb {
                    Some(mb) if mb > most => Err(too_much(mb, most)),
                    _ => Ok(()),
                },
            );
            if let Err(reason) = checked {
                // Its token is recorded as not applied (D6, Tokens), unless an
                // earlier send of the same commit applied.
                let lock = self.store.lock()?;
                let mut state = self.store.load(&lock)?;
                state
                    .tokens
                    .entry(req.token.clone())
                    .or_insert(CommitAnswer::NotApplied);
                self.store.save(&lock, &state)?;
                return Ok(CommitReply::Invalid {
                    id: job.id.clone(),
                    reason,
                });
            }
        }
        let lock = self.store.lock()?;
        let mut state = self.store.load(&lock)?;
        self.settle(&lock, &mut state)?;
        let (mut reply, effects) = state.apply_commit(&req);
        // A tombstoned commit applies nothing, and stores nothing either.
        if reply != CommitReply::NotApplied {
            for (job, tarball) in req.jobs.iter().zip(&tarballs) {
                self.store.put_tarball(&lock, &job.id, tarball)?;
            }
        }
        self.store.save(&lock, &state)?;
        for stop in effects.stop {
            self.store
                .write_cancel(&stop.id, &stop.run, stop.attempt, &stop.marker)?;
            self.containers
                .stop(&container_name(&stop.id, &stop.run, stop.attempt));
        }
        self.admit(&lock, &mut state)?;
        state.answer_starts(&req, &mut reply);
        self.store.save(&lock, &state)?;
        Ok(reply)
    }

    /// A poll (`status`): settling and an admission round, then the
    /// preflight's answer, token tombstones included.
    ///
    /// # Errors
    /// If the state or podman fails.
    pub fn status(&mut self, req: &PreflightRequest) -> std::io::Result<PreflightReply> {
        let versions = self.versions.clone();
        let (mut reply, attempts, state) = self.round(|state| {
            (
                state.preflight(req, &versions),
                last_attempts(state),
                state.clone(),
            )
        })?;
        self.fill(&mut reply, &attempts, &state);
        Ok(reply)
    }

    /// Fill each job of `reply` from its run's last attempt's files, give
    /// each queued one what it waits for ([`Self::waits_for`]), and the reply
    /// the state volume's free space.
    fn fill(
        &self,
        reply: &mut PreflightReply,
        attempts: &[(String, String, u32, bool)],
        state: &State,
    ) {
        reply.disk = self.disk();
        let waits = self.waits_for(state);
        let queued = reply
            .jobs
            .values_mut()
            .chain(reply.runs.iter_mut().map(|(_, j)| j));
        for job in queued.filter(|j| j.state == crate::proto::VmState::Queued) {
            job.waits_for = Some(waits.to_owned());
        }
        let jobs = reply.jobs.iter_mut().map(|(id, job)| (id.as_str(), job));
        let runs = reply
            .runs
            .iter_mut()
            .map(|(ask, job)| (ask.id.as_str(), job));
        for (id, job) in jobs.chain(runs) {
            let attempt = attempts
                .iter()
                .find(|(i, r, _, _)| i == id && Some(r) == job.run.as_ref());
            if let Some((id, run, number, running)) = attempt {
                self.progress(id, run, *number, *running, job);
            }
        }
    }

    /// What a queued run waits for: the hold to be released (D10), disk
    /// below the floor, memory (the last admission round found too little
    /// free, `short_of_memory`), the whole VM (the first run waiting asks
    /// for more than a slot has), or a VM slot.
    pub fn waits_for(&self, state: &State) -> &'static str {
        let low = self
            .store
            .free_bytes()
            .is_ok_and(|free| free < self.disk_floor);
        if self.store.stopping() {
            "the queue to be released"
        } else if low {
            "disk"
        } else if state.head_whole(self.containers.shares()) {
            "the whole VM"
        } else if state.short_of_memory {
            "memory"
        } else {
            "a VM slot"
        }
    }

    /// Fill a job from its attempt's files: its lane decisions and progress
    /// curve, and while it runs its time and counters (the run so far from
    /// `result.json`, the counters from the curve's last progress line, which
    /// counts generated states across a recovery as TLC's log does not).
    fn progress(&self, id: &str, run: &str, number: u32, running: bool, job: &mut VmJob) {
        let dir = self.store.attempt_dir(id, run, number);
        // A paused run shows its counters and time at its checkpoint.
        let paused = matches!(
            job.held,
            Some(crate::proto::Held::Paused { died: None, .. })
        );
        if let Ok(Some(text)) = crate::curve::text_in(&dir) {
            if running || paused {
                job.counters = Curve::parse(&text).counters();
            }
            job.curve = Some(AttemptCurve {
                attempt: number,
                text,
            });
        }
        if let Ok(Some(result)) = crate::files::read_json::<AttemptResult>(&dir.join("result.json"))
        {
            job.lanes = result.lanes;
            if running {
                job.elapsed_secs = Some(result.duration_secs);
                job.left_secs = result.left_secs.zip(result.cap_secs);
                job.started_at = result.started_at;
            }
            if let Some(crate::proto::Held::Paused { at, .. }) = &mut job.held {
                *at = result.ended_at;
            }
        }
        if let Some(crate::proto::Held::Paused { checkpoint_mb, .. }) = &mut job.held {
            let dir = self.store.checkpoint_dir(id, run);
            *checkpoint_mb = Some(super::store::dir_bytes(&dir) >> 20);
        }
    }

    /// `cancel`: end the named run, or the id's live one, then admit. A queued
    /// run ends at once; a running one gets a cancel marker and its container
    /// is asked to stop. Returns that run's state after.
    ///
    /// # Errors
    /// If the state or podman fails.
    pub fn cancel(&mut self, req: &CancelRequest) -> std::io::Result<VmJob> {
        let lock = self.store.lock()?;
        let mut state = self.store.load(&lock)?;
        self.settle(&lock, &mut state)?;
        let run = req.run.clone().or_else(|| {
            state
                .latest(&req.id)
                .filter(|r| r.live())
                .map(|r| r.name.clone())
        });
        let marker = CancelMarker {
            by: req.by.clone(),
            replaced_by: None,
        };
        if let Some(run) = &run {
            if let Some(attempt) = state.cancel_run(&req.id, run, &marker) {
                self.store.save(&lock, &state)?;
                self.store.write_cancel(&req.id, run, attempt, &marker)?;
                self.containers.stop(&container_name(&req.id, run, attempt));
            }
        }
        self.admit(&lock, &mut state)?;
        self.store.save(&lock, &state)?;
        // The run asked about, which need not be the id's latest.
        Ok(match &run {
            Some(run) => state.run_job(&req.id, run),
            None => state.vm_job(&req.id),
        })
    }

    /// The runner's start, and its every-minute tick with `restop` false:
    /// settling, then for a start the stops of cancelled attempts again (a
    /// crash may have cut them off), then an admission round.
    ///
    /// # Errors
    /// If the state or podman fails.
    pub fn tick(&mut self, restop: bool) -> std::io::Result<()> {
        if restop {
            self.reconcile()?;
        }
        let stops = self.round(|state| {
            let mut stops = Vec::new();
            for (id, runs) in &state.runs {
                for run in runs {
                    let attempt = run.attempts.last().filter(|a| !a.ended);
                    if let (Some(a), Phase::Stopping { by, replaced_by }) = (attempt, &run.phase) {
                        let marker = CancelMarker {
                            by: by.clone(),
                            replaced_by: replaced_by.clone(),
                        };
                        stops.push((id.clone(), run.name.clone(), a.number, marker));
                    }
                }
            }
            stops
        })?;
        if restop {
            for (id, run, attempt, marker) in stops {
                self.store.write_cancel(&id, &run, attempt, &marker)?;
                self.containers.stop(&container_name(&id, &run, attempt));
            }
        }
        self.clean_up()
    }

    /// The state volume's free space and the floor, if it can be read.
    pub fn disk(&self) -> Option<crate::proto::Disk> {
        let free = self.store.free_bytes().ok()?;
        Some(crate::proto::Disk {
            free_mb: free >> 20,
            floor_mb: self.disk_floor >> 20,
        })
    }

    /// With the state volume below twice the disk floor, delete what nothing
    /// owns: the scratch and checkpoints no recorded run has, and old images.
    fn clean_up(&mut self) -> std::io::Result<()> {
        if !self.disk().is_some_and(|d| d.low()) {
            return Ok(());
        }
        let lock = self.store.lock()?;
        let state = self.store.load(&lock)?;
        let freed = self.store.delete_orphans(&state)?;
        drop(lock);
        if freed > 0 {
            eprintln!(
                "disk low: deleted {} MB of orphaned scratch and checkpoints",
                freed >> 20
            );
        }
        self.containers.prune_images();
        Ok(())
    }

    /// A human's reorder from the UI (D16), then an admission round. Returns
    /// whether the order changed.
    ///
    /// # Errors
    /// If the state or podman fails.
    pub fn reorder(&mut self, id: &str, run: &str, to: Move) -> std::io::Result<bool> {
        let lock = self.store.lock()?;
        let mut state = self.store.load(&lock)?;
        self.settle(&lock, &mut state)?;
        let moved = state.reorder(id, run, to);
        self.admit(&lock, &mut state)?;
        self.store.save(&lock, &state)?;
        Ok(moved)
    }

    /// A human's release of the held queue from the UI (D16): clear the
    /// stopping mark a hold or a SIGTERM left (D10), so the queue is admitted
    /// again, starting now.
    ///
    /// # Errors
    /// If the state, the mark or podman fails.
    pub fn release(&mut self) -> std::io::Result<()> {
        self.ui_verb(|runner, _| runner.store.set_stopping(false).map(|()| true))
            .map(|_| ())
    }

    /// A human's freeze of a running or thawed job that is not pausing
    /// (pinned-slots S4): its container paused, its slot freed, and held so:
    /// never thawed on spare capacity, until an unfreeze or its pause.
    /// Returns whether it froze.
    ///
    /// # Errors
    /// If the state, a file or podman fails.
    pub fn freeze(&mut self, id: &str, run: &str) -> std::io::Result<bool> {
        self.ui_verb(|runner, state| {
            let Some(a) = live_attempt(state, id, run)
                .filter(|a| matches!(a.freeze, Freeze::Running | Freeze::Thawed) && !a.pausing)
                .filter(|a| runner.can_act(id, run, a))
            else {
                return Ok(false);
            };
            let froze = runner.freeze_attempt(state, id, run, a.number);
            if let Some(a) = attempt_mut(state, id, run).filter(|_| froze) {
                a.held = true;
            }
            Ok(froze)
        })
    }

    /// A human's unfreeze (S4): a thawed job runs on in its slot, no longer
    /// frozen again for new work; a frozen one takes the next free slot
    /// before the queue. Returns whether it changed.
    ///
    /// # Errors
    /// If the state, a file or podman fails.
    pub fn unfreeze(&mut self, id: &str, run: &str) -> std::io::Result<bool> {
        self.ui_verb(|runner, state| {
            let live = state
                .runs
                .get(id)
                .and_then(|rs| rs.iter().find(|r| r.name == run))
                .is_some_and(|r| matches!(r.phase, Phase::Running { .. }));
            let acts = live_attempt(state, id, run).is_some_and(|a| runner.can_act(id, run, &a));
            let Some(a) = attempt_mut(state, id, run).filter(|a| live && acts && !a.pausing) else {
                return Ok(false);
            };
            a.freeze = match a.freeze {
                Freeze::Frozen => Freeze::Unfrozen,
                Freeze::Thawed => Freeze::Running,
                _ => return Ok(false),
            };
            a.held = false;
            Ok(true)
        })
    }

    /// A human's pause (S5): the request its supervisor stops it on at the
    /// engine's next checkpoint; a frozen job is unpaused for it and stops at
    /// once. Refused once its search has ended: no checkpoint comes, and tlx
    /// has deleted its last. Returns whether it was asked.
    ///
    /// # Errors
    /// If the state, a file or podman fails.
    pub fn pause(&mut self, id: &str, run: &str) -> std::io::Result<bool> {
        self.ui_verb(|runner, state| {
            let Some(a) = live_attempt(state, id, run).filter(|a| {
                let searched = runner.store.attempt_dir(id, run, a.number).join("searched");
                !a.pausing && runner.can_act(id, run, a) && !searched.exists()
            }) else {
                return Ok(false);
            };
            let number = a.number;
            runner.store.write_pause(id, run, number)?;
            let frozen = matches!(a.freeze, Freeze::Frozen | Freeze::Unfrozen);
            if frozen {
                let name = container_name(id, run, number);
                runner.store.log_frozen(id, run, number, "unpause", now())?;
                if let Err(e) = runner.containers.unpause(&name) {
                    // Undone whole: no request left for a later thaw to act on.
                    runner.store.log_frozen(id, run, number, "freeze", now())?;
                    runner.store.remove_pause(id, run, number)?;
                    return Err(std::io::Error::other(e));
                }
            }
            if let Some(a) = attempt_mut(state, id, run) {
                a.pausing = true;
                // A thawed one stays thawed: frozen again for new work once kept.
                if frozen {
                    a.freeze = Freeze::Unpaused;
                    a.held = false;
                }
            }
            Ok(true)
        })
    }

    /// A human's keep running: a pause withdrawn while the job pauses in its
    /// slot, before its supervisor marked it paused (it rechecks the request
    /// under the lock as it marks). Returns whether it was withdrawn.
    ///
    /// # Errors
    /// If the state or a file fails.
    pub fn keep(&mut self, id: &str, run: &str) -> std::io::Result<bool> {
        self.ui_verb(|runner, state| {
            let Some(a) = live_attempt(state, id, run).filter(|a| {
                let paused = runner.store.attempt_dir(id, run, a.number).join("paused");
                a.pausing
                    && matches!(a.freeze, Freeze::Running | Freeze::Thawed)
                    && runner.can_act(id, run, a)
                    && !paused.exists()
            }) else {
                return Ok(false);
            };
            runner.store.remove_pause(id, run, a.number)?;
            if let Some(a) = attempt_mut(state, id, run) {
                a.pausing = false;
            }
            Ok(true)
        })
    }

    /// A human's resume of a paused run (S5): its next attempt goes first in
    /// the queue and recovers the checkpoint. Returns whether it was resumed.
    ///
    /// # Errors
    /// If the state or podman fails.
    pub fn resume(&mut self, id: &str, run: &str) -> std::io::Result<bool> {
        self.ui_verb(|_, state| Ok(state.resume(id, run, (None, None))))
    }

    /// `resume`: an agent's resume of the named run, or the id's live one,
    /// when it is paused, asking for `memory_mb` and capped at `timeout_secs`
    /// when given (refused at or below its duration so far), then admit.
    /// Returns that run's state after, and why it was not resumed.
    ///
    /// # Errors
    /// If the state or podman fails.
    pub fn resume_run(&mut self, req: &ResumeRequest) -> std::io::Result<ResumeReply> {
        let lock = self.store.lock()?;
        let mut state = self.store.load(&lock)?;
        self.settle(&lock, &mut state)?;
        let run = req.run.clone().or_else(|| {
            state
                .latest(&req.id)
                .filter(|r| r.live())
                .map(|r| r.name.clone())
        });
        let most = self.containers.shares().whole_mb;
        let ran = run.as_ref().and_then(|name| {
            let runs = state.runs.get(&req.id)?;
            Some(runs.iter().find(|r| &r.name == name)?.duration_secs)
        });
        let refused = match (&run, req.memory_mb, req.timeout_secs.zip(ran)) {
            (None, ..) => Some("it has no live run on the VM".to_owned()),
            (_, Some(mb), _) if mb > most => Some(too_much(mb, most)),
            (_, _, Some((cap, ran))) if cap <= ran => Some(format!(
                "it has run {ran} s already: give it a --timeout above that"
            )),
            (Some(run), mb, _) if !state.resume(&req.id, run, (mb, req.timeout_secs)) => {
                Some("it is not paused on the VM".to_owned())
            }
            _ => None,
        };
        self.admit(&lock, &mut state)?;
        self.store.save(&lock, &state)?;
        let job = match &run {
            Some(run) => state.run_job(&req.id, run),
            None => state.vm_job(&req.id),
        };
        Ok(ResumeReply { job, refused })
    }

    /// A human's toggle of the P slot (S6): closed, nothing starts or thaws
    /// there, and a thawed job in it that is not pausing is frozen again.
    ///
    /// # Errors
    /// If the state, the mark or podman fails.
    pub fn set_p_open(&mut self, open: bool) -> std::io::Result<()> {
        self.ui_verb(|runner, state| {
            runner.store.set_p_open(open)?;
            if open {
                return Ok(true);
            }
            let thawed: Vec<(String, String, u32)> = state
                .runs
                .iter()
                .flat_map(|(id, rs)| rs.iter().map(move |r| (id, r)))
                .filter_map(|(id, r)| {
                    let a = r.live_attempt()?;
                    (a.freeze == Freeze::Thawed && a.slot == Slot::P && !a.pausing)
                        .then(|| (id.clone(), r.name.clone(), a.number))
                })
                .collect();
            for (id, run, number) in thawed {
                runner.freeze_attempt(state, &id, &run, number);
            }
            Ok(true)
        })
        .map(|_| ())
    }

    /// Whether the page may freeze or pause attempt `a`: it started and is
    /// not cancelled.
    fn can_act(&self, id: &str, run: &str, a: &Attempt) -> bool {
        let dir = self.store.attempt_dir(id, run, a.number);
        dir.join("started").exists() && !dir.join("cancel.json").exists()
    }

    /// Freeze attempt `number` of run `run` of `id`: pause its container, log
    /// it, and record it frozen. Returns whether it froze.
    fn freeze_attempt(&mut self, state: &mut State, id: &str, run: &str, number: u32) -> bool {
        let at = now();
        if pause_logged(&self.store, &mut self.containers, id, run, number, at).is_err() {
            return false;
        }
        if let Some(a) = attempt_mut(state, id, run) {
            (a.freeze, a.frozen_at) = (Freeze::Frozen, Some(at));
        }
        true
    }

    /// A UI verb under the lock: settle, `act`, admit, save.
    fn ui_verb(
        &mut self,
        act: impl FnOnce(&mut Self, &mut State) -> std::io::Result<bool>,
    ) -> std::io::Result<bool> {
        let lock = self.store.lock()?;
        let mut state = self.store.load(&lock)?;
        self.settle(&lock, &mut state)?;
        let done = act(self, &mut state)?;
        self.store.save(&lock, &state)?;
        self.admit(&lock, &mut state)?;
        self.store.save(&lock, &state)?;
        Ok(done)
    }

    /// `ls`: every id's state, gone attempts reported as settling would.
    ///
    /// # Errors
    /// If the state or podman cannot be read.
    pub fn ls(&self) -> std::io::Result<BTreeMap<String, VmJob>> {
        let lock = self.store.lock()?;
        let mut view = self.store.load(&lock)?;
        self.settle_view(&mut view)?;
        Ok(view
            .runs
            .keys()
            .map(|id| (id.clone(), view.vm_job(id)))
            .collect())
    }

    /// `fetch`: a run's state and its recorded attempts' `result.json` and TLC
    /// log, listed under the lock and streamed to `out` after it: the reply line,
    /// then each file's bytes. A run with no recorded attempt has no files.
    ///
    /// # Errors
    /// If the state, podman or a file cannot be read, or `out` written.
    pub fn fetch(&self, req: &FetchRequest, out: &mut impl Write) -> std::io::Result<()> {
        let lock = self.store.lock()?;
        let mut view = self.store.load(&lock)?;
        self.settle_view(&mut view)?;
        let run = req
            .run
            .clone()
            .or_else(|| view.latest(&req.id).map(|r| r.name.clone()))
            .unwrap_or_default();
        let mut files = Vec::new();
        let mut paths = Vec::new();
        let attempts = view
            .runs
            .get(&req.id)
            .and_then(|rs| rs.iter().find(|r| r.name == run));
        for a in attempts.map_or(&[][..], |r| r.attempts.as_slice()) {
            for name in ["result.json", "tlc.log"] {
                let path = self.store.attempt_dir(&req.id, &run, a.number).join(name);
                if let Ok(meta) = std::fs::metadata(&path) {
                    files.push(FetchFile {
                        attempt: a.number,
                        name: name.to_owned(),
                        len: meta.len(),
                    });
                    paths.push(path);
                }
            }
        }
        let reply = FetchReply {
            job: view.run_job(&req.id, &run),
            files,
        };
        drop(lock);
        serde_json::to_writer(&mut *out, &reply)?;
        out.write_all(b"\n")?;
        for (path, file) in paths.iter().zip(&reply.files) {
            let copied = std::io::copy(&mut std::fs::File::open(path)?.take(file.len), out)?;
            if copied < file.len {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
            }
        }
        Ok(())
    }

    /// At the runner's start, before any admission: make each live attempt's
    /// freezing what its container is, should a runner killed between pausing
    /// or unpausing a container and saving the state have left them apart. A
    /// container paused while the state says it runs is frozen; one running
    /// while the state says frozen runs again, in the slot its last thaw logged.
    ///
    /// # Errors
    /// If the state, a log or podman fails.
    fn reconcile(&mut self) -> std::io::Result<()> {
        let lock = self.store.lock()?;
        let mut state = self.store.load(&lock)?;
        self.settle(&lock, &mut state)?;
        let paused = self.containers.paused()?;
        let running = self.containers.running()?;
        let at = now();
        let mut changed = false;
        for (id, runs) in &mut state.runs {
            for run in runs.iter_mut() {
                let name = run.name.clone();
                let Some(a) = run.attempts.last_mut().filter(|a| !a.ended) else {
                    continue;
                };
                let container = container_name(id, &name, a.number);
                let is_paused = paused.contains(&container);
                if !is_paused && !running.contains(&container) {
                    continue;
                }
                let log = self.store.frozen_log(id, &name, a.number)?;
                match (a.freeze, is_paused) {
                    (Freeze::Running | Freeze::Thawed, true) => {
                        (a.freeze, a.frozen_at) = (Freeze::Frozen, Some(at));
                        changed = true;
                    }
                    (Freeze::Frozen | Freeze::Unfrozen, false) => {
                        a.slot = last_thaw(&log).unwrap_or(a.slot);
                        a.freeze = if a.freeze == Freeze::Frozen {
                            Freeze::Thawed
                        } else {
                            Freeze::Running
                        };
                        changed = true;
                    }
                    _ => {}
                }
                // The log follows the container too, so its supervisor's clock does.
                let logged_frozen = log
                    .lines()
                    .last()
                    .is_some_and(|l| l.split_whitespace().nth(1) == Some("freeze"));
                if is_paused != logged_frozen {
                    let event = match (is_paused, a.slot) {
                        (true, _) => "freeze",
                        (false, Slot::E) => "thaw e",
                        (false, Slot::P) => "thaw p",
                    };
                    self.store.log_frozen(id, &name, a.number, event, at)?;
                }
            }
        }
        if changed {
            self.store.save(&lock, &state)?;
        }
        Ok(())
    }

    /// Under the lock: settle, admit, apply `then` to the state, and save.
    fn round<T>(&mut self, then: impl FnOnce(&mut State) -> T) -> std::io::Result<T> {
        let lock = self.store.lock()?;
        let mut state = self.store.load(&lock)?;
        self.settle(&lock, &mut state)?;
        self.admit(&lock, &mut state)?;
        let out = then(&mut state);
        self.store.save(&lock, &state)?;
        Ok(out)
    }

    /// Settle, in `state`, every recorded attempt whose container is gone and
    /// that has no end. Returns what settling did to each.
    fn settle_view(
        &self,
        state: &mut State,
    ) -> std::io::Result<Vec<(String, String, u32, Settled)>> {
        let mut settled = Vec::new();
        for (id, run, number, _) in self.gone(state)? {
            let seen = self.store.observe(&id, &run, number)?;
            let most = self.containers.shares().whole_mb;
            let what = state.settle(&id, &run, number, &seen, (self.never_started, most));
            settled.push((id, run, number, what));
        }
        Ok(settled)
    }

    /// Every recorded attempt with no end whose container is gone, and its slot.
    fn gone(&self, state: &State) -> std::io::Result<Vec<(String, String, u32, Slot)>> {
        let running = self.containers.running()?;
        let mut gone = Vec::new();
        for (id, runs) in &state.runs {
            for run in runs {
                if let Some(a) = run.attempts.last().filter(|a| !a.ended) {
                    if !running.contains(&container_name(id, &run.name, a.number)) {
                        gone.push((id.clone(), run.name.clone(), a.number, a.slot));
                    }
                }
            }
        }
        Ok(gone)
    }

    /// Keep the checkpoint of a gone attempt of a long run that its supervisor
    /// did not keep: one stopped at its cap by a supervisor that kept nothing
    /// then (an older one), or one whose supervisor died with its engine (a
    /// shutdown, a reboot) and never wrote its end. Settling then pauses the
    /// run, as for a stop the supervisor kept. With no tlx binary to keep, a
    /// resume runs the image's.
    fn rescue(&self, id: &str, run: &str, number: u32, slot: Slot) -> std::io::Result<()> {
        let dir = self.store.attempt_dir(id, run, number);
        let result: AttemptResult =
            crate::files::read_json(&dir.join("result.json"))?.unwrap_or_default();
        let [started, cancel, paused, timed_out, no_checkpoint] = [
            "started",
            "cancel.json",
            "paused",
            "timed-out",
            "no-checkpoint",
        ]
        .map(|m| dir.join(m).exists());
        let kept = self.store.checkpoint(id, run)?;
        if !started
            || cancel
            || paused
            || no_checkpoint
            || (result.ended_at.is_some() && !timed_out)
            || result.duration_secs < super::exec::KEEP_AFTER_SECS
            || kept.is_some_and(|c| c.attempt == Some(number))
        {
            return Ok(());
        }
        let workers = self.containers.workers(slot);
        let generated = kept.and_then(|c| c.generated);
        super::exec::keep_checkpoint(
            &self.store,
            (id, run, number),
            &dir,
            workers,
            None,
            generated,
        )
    }

    /// Settle gone attempts, save, then delete their directories or scratch and
    /// their containers, and sweep the scratch of every ended attempt and the
    /// checkpoint of every ended run.
    fn settle(&mut self, lock: &AdmissionLock, state: &mut State) -> std::io::Result<()> {
        for (id, run, number, slot) in self.gone(state)? {
            self.rescue(&id, &run, number, slot)?;
        }
        let settled = self.settle_view(state)?;
        if !settled.is_empty() {
            self.store.save(lock, state)?;
        }
        for (id, run, number, what) in settled {
            if what == Settled::Undone {
                self.store.delete_attempt(&id, &run, number)?;
            }
            let end = state
                .runs
                .get(&id)
                .and_then(|runs| runs.iter().find(|r| r.name == run))
                .and_then(|r| match &r.phase {
                    Phase::Ended { result } if what == Settled::Ended => Some(result),
                    _ => None,
                });
            if let Some(end) = end {
                self.store.record_end(&id, &run, number, end)?;
            }
            self.containers.remove(&container_name(&id, &run, number));
        }
        for (id, runs) in &state.runs {
            for run in runs {
                for a in run.attempts.iter().filter(|a| a.ended) {
                    self.store.delete_scratch(id, &run.name, a.number)?;
                }
                if matches!(run.phase, Phase::Ended { .. }) {
                    self.store.delete_checkpoint(id, &run.name)?;
                }
            }
        }
        Ok(())
    }

    /// One admission round, unless the runner is stopping or the state volume
    /// is below the disk floor.
    fn admit(&mut self, lock: &AdmissionLock, state: &mut State) -> std::io::Result<()> {
        if self.store.stopping() || self.store.free_bytes()? < self.disk_floor {
            return Ok(());
        }
        let at = now();
        let mut launch = Launcher {
            store: &self.store,
            lock,
            containers: &mut self.containers,
            now: at,
        };
        state.admit(self.store.p_open(), at, &mut launch)?;
        Ok(())
    }
}

/// Pause attempt `number`'s container, its freeze logged first at `at`, so
/// the log never lags a paused container; a pause that fails is logged
/// undone.
fn pause_logged<C: Containers>(
    store: &Store,
    containers: &mut C,
    id: &str,
    run: &str,
    number: u32,
    at: u64,
) -> Result<(), String> {
    store
        .log_frozen(id, run, number, "freeze", at)
        .map_err(|e| e.to_string())?;
    containers
        .pause(&container_name(id, run, number))
        .inspect_err(|_| {
            if let Err(e) = store.log_frozen(id, run, number, "thaw", at) {
                eprintln!("frozen.log of {id} {run} {number}: {e}");
            }
        })
}

/// The slot an attempt's `frozen.log` last thawed it into, if any.
fn last_thaw(log: &str) -> Option<Slot> {
    log.lines().rev().find_map(
        |l| match l.split_whitespace().skip(1).collect::<Vec<_>>()[..] {
            ["thaw", "e"] => Some(Slot::E),
            ["thaw", "p"] => Some(Slot::P),
            _ => None,
        },
    )
}

/// The current time, in seconds since the epoch.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A copy of run `run` of `id`'s attempt with no end, if any.
fn live_attempt(state: &State, id: &str, run: &str) -> Option<Attempt> {
    state
        .runs
        .get(id)?
        .iter()
        .find(|r| r.name == run)?
        .live_attempt()
        .cloned()
}

/// Run `run` of `id`'s attempt with no end, if any.
fn attempt_mut<'a>(state: &'a mut State, id: &str, run: &str) -> Option<&'a mut Attempt> {
    state
        .runs
        .get_mut(id)?
        .iter_mut()
        .find(|r| r.name == run)?
        .attempts
        .last_mut()
        .filter(|a| !a.ended)
}

/// Admission's view of the runner: the state saved under the lock, an
/// attempt's directory created before its container starts, freezes and
/// thaws logged at `now`.
struct Launcher<'a, C> {
    store: &'a Store,
    lock: &'a AdmissionLock,
    containers: &'a mut C,
    now: u64,
}

impl<C: Containers> Launch for Launcher<'_, C> {
    fn save(&mut self, state: &State) -> std::io::Result<()> {
        self.store.save(self.lock, state)
    }

    fn memory(&mut self, slot: Slot, alone: bool, frozen: &BTreeSet<String>) -> Option<u64> {
        self.containers.memory(slot, alone, frozen)
    }

    fn shares(&self) -> super::admit::Shares {
        self.containers.shares()
    }

    fn freeze(&mut self, id: &str, run: &str, number: u32) -> Result<(), String> {
        pause_logged(self.store, self.containers, id, run, number, self.now)
    }

    fn thaw(&mut self, id: &str, run: &str, number: u32, slot: Slot) -> Result<(), String> {
        let name = container_name(id, run, number);
        self.containers.move_to(&name, slot)?;
        let event = match slot {
            Slot::E => "thaw e",
            Slot::P => "thaw p",
        };
        // Logged first, so its supervisor counts the time from its thaw on.
        self.store
            .log_frozen(id, run, number, event, self.now)
            .map_err(|e| e.to_string())?;
        self.containers.unpause(&name).inspect_err(|_| {
            // Best effort: a thaw logged but not done overcounts its running time only.
            if let Err(e) = self.store.log_frozen(id, run, number, "freeze", self.now) {
                eprintln!("frozen.log of {name}: {e}");
            }
        })
    }

    fn start(
        &mut self,
        id: &str,
        run: &Run,
        attempt: &Attempt,
        memory_mb: u64,
    ) -> Result<(), String> {
        let first = self.first_result(id, run).map_err(|e| e.to_string())?;
        let number = attempt.number;
        self.store
            .create_attempt(id, &run.name, number, &first)
            .map_err(|e| e.to_string())?;
        let name = container_name(id, &run.name, number);
        let started = self.containers.start(&name, id, run, attempt, memory_mb);
        if started.is_err() {
            self.store
                .delete_attempt(id, &run.name, number)
                .map_err(|e| e.to_string())?;
        }
        started
    }
}

impl<C> Launcher<'_, C> {
    /// An attempt's first `result.json`: the run so far, from its previous VM
    /// attempt, or for its first one the laptop's time from `job.json`.
    fn first_result(&self, id: &str, run: &Run) -> std::io::Result<AttemptResult> {
        let previous = run.attempts.iter().rev().nth(1);
        let Some(previous) = previous else {
            return Ok(AttemptResult {
                duration_secs: run.info.laptop_secs,
                ..AttemptResult::default()
            });
        };
        let path = self
            .store
            .attempt_dir(id, &run.name, previous.number)
            .join("result.json");
        let previous: AttemptResult = crate::files::read_json(&path)?.unwrap_or_default();
        Ok(AttemptResult {
            finished: None,
            ..previous
        })
    }
}

/// Each run's last attempt: its id, run, number, and whether it runs.
fn last_attempts(state: &State) -> Vec<(String, String, u32, bool)> {
    state
        .runs
        .iter()
        .flat_map(|(id, runs)| runs.iter().map(move |r| (id, r)))
        .filter_map(|(id, r)| {
            let a = r.attempts.last()?;
            Some((id.clone(), r.name.clone(), a.number, !a.ended))
        })
        .collect()
}

#[cfg(test)]
#[path = "verbs_tests.rs"]
pub(crate) mod tests;

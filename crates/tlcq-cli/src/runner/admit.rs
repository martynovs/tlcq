//! Admission (design D9, pinned-slots S2, S4): which waiting run takes which
//! slot, first in, first out, the E slot first, and the round that records and
//! starts attempts, thaws frozen jobs on spare capacity and freezes them again
//! for new work.
//!
//! Slot use is derived, never stored: a slot is busy while a recorded attempt
//! with no end runs in it, or a thawed one does; a frozen attempt holds none.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::state::{Attempt, Freeze, Phase, Run, Slot, State};
use super::verbs::container_name;

/// What a round needs from outside the state: to save it, size memory, and
/// start, freeze and thaw attempts' containers.
pub trait Launch {
    /// Save the state: an attempt is recorded on disk before its container starts.
    ///
    /// # Errors
    /// If the state cannot be written.
    fn save(&mut self, state: &State) -> std::io::Result<()>;

    /// The memory cap, in MB, a new attempt in `slot` would get now, `alone`
    /// with the P slot closed, the `frozen` containers' unused limits not
    /// promised; `None` if too little is free.
    fn memory(&mut self, slot: Slot, alone: bool, frozen: &BTreeSet<String>) -> Option<u64>;

    /// The most memory a slot's attempt can get, with both slots open and
    /// with only the E slot open.
    fn shares(&self) -> Shares;

    /// Create the attempt's directory and first `result.json`, and start its
    /// container with `memory_mb` as its cap; on failure, delete what it
    /// created.
    ///
    /// # Errors
    /// Why the container did not start.
    fn start(
        &mut self,
        id: &str,
        run: &Run,
        attempt: &Attempt,
        memory_mb: u64,
    ) -> Result<(), String>;

    /// Pause attempt `number`'s container and log the freeze.
    ///
    /// # Errors
    /// Why it was not paused.
    fn freeze(&mut self, id: &str, run: &str, number: u32) -> Result<(), String>;

    /// Move attempt `number`'s paused container to `slot`'s CPUs, unpause it
    /// and log the thaw.
    ///
    /// # Errors
    /// Why it was not thawed.
    fn thaw(&mut self, id: &str, run: &str, number: u32, slot: Slot) -> Result<(), String>;
}

/// The most memory, in MB, an attempt can get: in a slot with both slots
/// open, and in the E slot alone (the VM's memory less the reserve).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Shares {
    /// With both slots open.
    pub slot_mb: u64,
    /// With only the E slot open: the most the VM can ever give.
    pub whole_mb: u64,
}

impl Shares {
    /// Whether a run asking for `request` MB needs the whole VM (job-memory M2).
    pub fn whole(&self, request: Option<u64>) -> bool {
        request.is_some_and(|r| r > self.slot_mb)
    }
}

/// Where a human moves a queued run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Move {
    /// Ahead of the queued run before it.
    Up,
    /// Behind the queued run after it.
    Down,
    /// Ahead of every queued run.
    Head,
}

/// What waits for a slot, first in the queue first.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Waiter {
    /// A run's next attempt: a resume's, or a queued run's first.
    Run(String, String),
    /// An unfrozen job's frozen attempt.
    Unfrozen(String, String, u32),
}

impl State {
    /// The slots that hold a job: running or thawed.
    pub fn held_slots(&self) -> BTreeSet<Slot> {
        self.runs
            .values()
            .flatten()
            .filter_map(|r| r.live_attempt()?.holds())
            .collect()
    }

    /// The open slots no job holds, E first.
    pub fn free_slots(&self, p_open: bool) -> Vec<Slot> {
        let held = self.held_slots();
        Slot::ALL
            .into_iter()
            .filter(|s| (*s == Slot::E || p_open) && !held.contains(s))
            .collect()
    }

    /// The queue's live runs, in order.
    fn in_order(&self) -> impl Iterator<Item = (&String, &Run)> {
        self.queue.iter().filter_map(|(id, name)| {
            Some((id, self.runs.get(id)?.iter().find(|r| &r.name == name)?))
        })
    }

    /// What waits, in order: queued resumes and unfrozen jobs, in queue order,
    /// then the queued runs.
    fn waiters(&self) -> Vec<Waiter> {
        let mut first: Vec<Waiter> = self
            .in_order()
            .filter_map(|(id, r)| match r.live_attempt() {
                None if r.resume => Some(Waiter::Run(id.clone(), r.name.clone())),
                Some(a) if a.freeze == Freeze::Unfrozen => {
                    Some(Waiter::Unfrozen(id.clone(), r.name.clone(), a.number))
                }
                _ => None,
            })
            .collect();
        first.extend(
            self.in_order()
                .filter(|(_, r)| r.phase == Phase::Queued)
                .map(|(id, r)| Waiter::Run(id.clone(), r.name.clone())),
        );
        first
    }

    /// The live attempts whose `freeze` is one of `which`, in queue order.
    fn attempts_in(&self, which: &[Freeze]) -> Vec<(String, String, Attempt)> {
        self.in_order()
            .filter_map(|(id, r)| {
                let a = r.live_attempt().filter(|a| which.contains(&a.freeze))?;
                Some((id.clone(), r.name.clone(), a.clone()))
            })
            .collect()
    }

    /// The containers paused now: frozen jobs, unfrozen ones still waiting.
    pub fn frozen_containers(&self) -> BTreeSet<String> {
        self.attempts_in(&[Freeze::Frozen, Freeze::Unfrozen])
            .into_iter()
            .map(|(id, run, a)| container_name(&id, &run, a.number))
            .collect()
    }

    /// One admission round: while an open slot is free, the first waiting
    /// run starts in it (an unfrozen job thaws into it), or with nothing
    /// waiting the oldest frozen job a human did not freeze thaws into it on
    /// spare capacity; while none is free and something waits, a thawed job
    /// is frozen again for it.
    /// A whole-VM run live, or first in the queue, holds the P slot: nothing
    /// starts or thaws there, and a thawed job in it is frozen again for the
    /// run waiting (job-memory M3, M4). Too little memory, like a failed
    /// start (which undoes its record), ends the round. Returns the runs started.
    ///
    /// # Errors
    /// If the state cannot be saved.
    pub fn admit(
        &mut self,
        p_open: bool,
        now: u64,
        launch: &mut impl Launch,
    ) -> std::io::Result<Vec<(String, String)>> {
        let mut started = Vec::new();
        let short = std::mem::take(&mut self.short_of_memory);
        let shares = launch.shares();
        loop {
            let frozen = self.frozen_containers();
            let waiters = self.waiters();
            let head_whole = waiters
                .first()
                .is_some_and(|w| shares.whole(self.request(w)));
            if head_whole {
                let in_p = self
                    .attempts_in(&[Freeze::Thawed])
                    .into_iter()
                    .find(|(_, _, a)| a.slot == Slot::P && !a.pausing);
                if let Some((id, name, _)) = in_p {
                    if !self.refreeze(&id, &name, now, launch)? {
                        break;
                    }
                    continue;
                }
            }
            let p_open = p_open && !head_whole && !self.holds_whole();
            // A frozen job's memory is in use already: a thaw needs only the slot.
            let spare = self
                .attempts_in(&[Freeze::Frozen])
                .into_iter()
                .find(|(_, _, a)| !a.held);
            let step = match self.free_slots(p_open).first().copied() {
                Some(slot) if !waiters.is_empty() => {
                    match self.pick(launch, shares, &waiters, slot, !p_open, &frozen) {
                        Some((Waiter::Run(id, name), Some(memory_mb))) => {
                            let whole = shares.whole(self.run_mut(&id, &name).info.memory_mb);
                            let ok = self.start(&id, &name, slot, (memory_mb, whole), launch)?;
                            if ok {
                                started.push((id, name));
                            }
                            ok
                        }
                        Some((Waiter::Unfrozen(id, name, number), _)) => {
                            self.thaw(&id, &name, number, slot, Freeze::Running, launch)?
                        }
                        _ => {
                            // The run waiting is short of memory: the slot
                            // runs a frozen job meanwhile, frozen again once
                            // the run can start.
                            self.short_of_memory = true;
                            let Some((id, name, a)) = spare else {
                                break;
                            };
                            self.thaw(&id, &name, a.number, slot, Freeze::Thawed, launch)?
                        }
                    }
                }
                Some(slot) => {
                    let Some((id, name, a)) = spare else {
                        break;
                    };
                    self.thaw(&id, &name, a.number, slot, Freeze::Thawed, launch)?
                }
                None if !waiters.is_empty() => {
                    let Some((id, name, a)) = self
                        .attempts_in(&[Freeze::Thawed])
                        .into_iter()
                        .find(|(_, _, a)| !a.pausing && (a.slot == Slot::E || p_open))
                    else {
                        break;
                    };
                    let mut freed = frozen.clone();
                    freed.insert(container_name(&id, &name, a.number));
                    // Only for a waiter that would then take the slot.
                    if self
                        .pick(launch, shares, &waiters, a.slot, !p_open, &freed)
                        .is_none()
                    {
                        self.short_of_memory = true;
                        break;
                    }
                    self.refreeze(&id, &name, now, launch)?
                }
                None => break,
            };
            if !step {
                break;
            }
        }
        if self.short_of_memory != short {
            launch.save(self)?;
        }
        Ok(started)
    }

    /// Whether the first waiting run is a whole-VM run (`shares`): what the
    /// queue waits for is the whole VM.
    pub fn head_whole(&self, shares: Shares) -> bool {
        self.waiters()
            .first()
            .is_some_and(|w| shares.whole(self.request(w)))
    }

    /// Whether a whole-VM run has a live attempt: it holds the P slot.
    fn holds_whole(&self) -> bool {
        self.runs
            .values()
            .flatten()
            .any(|r| r.live_attempt().is_some_and(|a| a.whole))
    }

    /// A waiter's memory request: a run's, from its first submitter.
    fn request(&self, w: &Waiter) -> Option<u64> {
        let Waiter::Run(id, name) = w else {
            return None;
        };
        self.runs
            .get(id)?
            .iter()
            .find(|r| &r.name == name)?
            .info
            .memory_mb
    }

    /// The first of `waiters`, if it can take `slot` now (`alone` with P
    /// closed, the `frozen` containers' unused limits not promised), with a
    /// run's memory cap. An unfrozen job always can (its memory is in use
    /// already); a run whose request the slot's memory does not reach, or a
    /// whole-VM run anywhere but E with nothing holding P, cannot; and it
    /// blocks those behind it: first in, first out.
    fn pick(
        &self,
        launch: &mut impl Launch,
        shares: Shares,
        waiters: &[Waiter],
        slot: Slot,
        alone: bool,
        frozen: &BTreeSet<String>,
    ) -> Option<(Waiter, Option<u64>)> {
        let w = waiters.first()?;
        if matches!(w, Waiter::Unfrozen(..)) {
            return Some((w.clone(), None));
        }
        let request = self.request(w);
        let whole = shares.whole(request);
        if whole && (slot != Slot::E || self.held_slots().contains(&Slot::P)) {
            return None;
        }
        launch
            .memory(slot, alone || whole, frozen)
            .filter(|&mb| request.is_none_or(|r| mb >= r))
            .map(|mb| (w.clone(), Some(mb)))
    }

    /// Record run `name` of `id`'s next attempt in `slot`, with its memory
    /// and whether it holds the whole VM, and start it. Returns whether it
    /// started.
    fn start(
        &mut self,
        id: &str,
        name: &str,
        slot: Slot,
        (memory_mb, whole): (u64, bool),
        launch: &mut impl Launch,
    ) -> std::io::Result<bool> {
        let run = self.run_mut(id, name);
        run.last_attempt += 1;
        let mut attempt = Attempt::new(run.last_attempt, slot, std::mem::take(&mut run.resume));
        attempt.memory_mb = Some(memory_mb);
        attempt.whole = whole;
        run.attempts.push(attempt.clone());
        run.phase = Phase::Running {
            place: slot.place().to_owned(),
            elapsed_secs: 0,
            counters: None,
        };
        launch.save(self)?;
        let run = self.run_mut(id, name);
        if launch.start(id, run, &attempt, memory_mb).is_err() {
            run.attempts.pop();
            run.resume = attempt.resume;
            if !attempt.resume {
                run.phase = Phase::Queued;
            }
            launch.save(self)?;
            return Ok(false);
        }
        Ok(true)
    }

    /// Thaw attempt `number` of run `name` of `id` into `slot`, as `to`
    /// (running, unfrozen; or thawed, on spare capacity). Returns whether it
    /// thawed.
    fn thaw(
        &mut self,
        id: &str,
        name: &str,
        number: u32,
        slot: Slot,
        to: Freeze,
        launch: &mut impl Launch,
    ) -> std::io::Result<bool> {
        if launch.thaw(id, name, number, slot).is_err() {
            return Ok(false);
        }
        let run = self.run_mut(id, name);
        if let Some(a) = run.attempts.last_mut() {
            (a.freeze, a.slot) = (to, slot);
        }
        if let Phase::Running { place, .. } = &mut run.phase {
            slot.place().clone_into(place);
        }
        launch.save(self)?;
        Ok(true)
    }

    /// Freeze run `name` of `id`'s thawed attempt again. Returns whether it froze.
    fn refreeze(
        &mut self,
        id: &str,
        name: &str,
        now: u64,
        launch: &mut impl Launch,
    ) -> std::io::Result<bool> {
        let Some(number) = self.run_mut(id, name).live_attempt().map(|a| a.number) else {
            return Ok(false);
        };
        if launch.freeze(id, name, number).is_err() {
            return Ok(false);
        }
        if let Some(a) = self.run_mut(id, name).attempts.last_mut() {
            (a.freeze, a.frozen_at) = (Freeze::Frozen, Some(now));
        }
        launch.save(self)?;
        Ok(true)
    }

    /// A human's move of the queued run `name` of `id` (D16): past one other
    /// queued run, or ahead of them all. Only queued runs move, among the queued
    /// runs' places; a running, frozen, paused or resumed run keeps its own.
    /// Returns whether the order changed.
    pub fn reorder(&mut self, id: &str, name: &str, to: Move) -> bool {
        let runs = &self.runs;
        let queued = |(i, n): &(String, String)| {
            runs.get(i)
                .and_then(|rs| rs.iter().find(|r| &r.name == n))
                .is_some_and(|r| r.phase == Phase::Queued)
        };
        let mut order: Vec<(String, String)> =
            self.queue.iter().filter(|e| queued(e)).cloned().collect();
        let Some(from) = order.iter().position(|(i, n)| i == id && n == name) else {
            return false;
        };
        let entry = order.remove(from);
        let at = match to {
            Move::Up => from.saturating_sub(1),
            Move::Down => (from + 1).min(order.len()),
            Move::Head => 0,
        };
        order.insert(at, entry);
        let mut order = order.into_iter();
        for slot in self.queue.iter_mut().filter(|e| queued(e)) {
            if let Some(e) = order.next() {
                *slot = e;
            }
        }
        at != from
    }

    /// The run `name` of `id`, which the caller knows exists.
    pub(crate) fn run_mut(&mut self, id: &str, name: &str) -> &mut Run {
        self.runs
            .get_mut(id)
            .and_then(|runs| runs.iter_mut().find(|r| r.name == name))
            .expect("a queued run exists")
    }
}

#[cfg(test)]
#[path = "admit_tests.rs"]
mod tests;

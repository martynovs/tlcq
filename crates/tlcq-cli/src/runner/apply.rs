//! Applying a commit to the runner's state (design D6, Commit), under the
//! admission lock, once every job has passed the VM's checks.
//!
//! In order: the pending entries, each onto its own run only; the jobs,
//! idempotently by id (attached if live, answered from the cache, else queued as
//! a new run); the supersedes (after the jobs, so a job a new entry has just
//! attached to survives). The admission round comes after, in the verb.

use std::collections::BTreeMap;

use super::state::{Phase, Run, State};
use super::store::CancelMarker;
use crate::commit::{
    CommitReply, CommitRequest, CommitRoute, JobAnswer, PendingFlush, SupersedeOrder,
};
use crate::entry::Entry;
use crate::proto::{CommitAnswer, RunRef, TokenAnswer};

/// What applying a commit leaves for the verb to do outside the state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Effects {
    /// Attempts a supersede cancelled while they ran.
    pub stop: Vec<Stop>,
}

/// A running attempt to cancel: write its cancel marker, then stop its container
/// without waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    /// The job.
    pub id: String,
    /// The run.
    pub run: String,
    /// The attempt.
    pub attempt: u32,
    /// The marker to write.
    pub marker: CancelMarker,
}

impl State {
    /// Apply `req`. A tombstoned token applies nothing; a token already applied
    /// answers as it did.
    pub fn apply_commit(&mut self, req: &CommitRequest) -> (CommitReply, Effects) {
        match self.tokens.get(&req.token) {
            Some(CommitAnswer::NotApplied) => return (CommitReply::NotApplied, Effects::default()),
            Some(CommitAnswer::Applied { jobs }) => {
                return (replay(jobs), Effects::default());
            }
            None => {}
        }
        let dropped = req
            .pending
            .iter()
            .filter(|p| !self.flush(p))
            .cloned()
            .collect();
        let mut answers = BTreeMap::new();
        for job in &req.jobs {
            answers.insert(job.id.clone(), self.place(job));
        }
        let mut effects = Effects::default();
        for order in &req.supersede {
            if let Some(stop) = self.supersede(order) {
                effects.stop.push(stop);
            }
        }
        let tokens = answers
            .iter()
            .map(|(id, a)| (id.clone(), token_answer(a)))
            .collect();
        self.tokens
            .insert(req.token.clone(), CommitAnswer::Applied { jobs: tokens });
        (
            CommitReply::Applied {
                jobs: answers,
                dropped,
                views: BTreeMap::new(),
            },
            effects,
        )
    }

    /// After the commit's admission round: each run it queued that started is
    /// answered started; a `now-or-refuse` one that did not is removed and
    /// answered refused, and its token records it as not applied.
    pub fn answer_starts(&mut self, req: &CommitRequest, reply: &mut CommitReply) {
        let CommitReply::Applied { jobs, views, .. } = reply else {
            return;
        };
        for job in &req.jobs {
            let Some(JobAnswer::Queued { run }) = jobs.get(&job.id) else {
                continue;
            };
            let run = run.clone();
            let answer = if self
                .latest(&job.id)
                .is_some_and(|r| r.phase != Phase::Queued)
            {
                JobAnswer::Started { run }
            } else if job.route == CommitRoute::NowOrRefuse {
                self.queue.retain(|(i, n)| !(i == &job.id && n == &run));
                if let Some(runs) = self.runs.get_mut(&job.id) {
                    runs.retain(|r| r.name != run);
                    if runs.is_empty() {
                        self.runs.remove(&job.id);
                    }
                }
                JobAnswer::Refused
            } else {
                continue;
            };
            if let Some(CommitAnswer::Applied { jobs: tokens }) = self.tokens.get_mut(&req.token) {
                tokens.insert(job.id.clone(), token_answer(&answer));
            }
            jobs.insert(job.id.clone(), answer);
        }
        for id in jobs.keys() {
            views.insert(id.clone(), self.vm_job(id));
        }
    }

    /// Add a pending entry to the run it names, if that run is live. Returns
    /// whether it was added.
    fn flush(&mut self, p: &PendingFlush) -> bool {
        let name = match &p.run {
            RunRef::Run(name) => Some(name.clone()),
            RunRef::Token(t) => match self.tokens.get(t) {
                Some(CommitAnswer::Applied { jobs }) => match jobs.get(&p.id) {
                    Some(TokenAnswer::Reached { run }) => Some(run.clone()),
                    _ => None,
                },
                _ => None,
            },
        };
        let Some(run) = name.and_then(|n| self.live_run(&p.id, &n)) else {
            return false;
        };
        add_entries(run, std::slice::from_ref(&p.entry));
        true
    }

    /// The live run `name` of `id`.
    fn live_run(&mut self, id: &str, name: &str) -> Option<&mut Run> {
        self.runs
            .get_mut(id)?
            .iter_mut()
            .find(|r| r.name == name && r.live())
    }

    /// Place one job: attach to its live run, answer from the cache, or queue a
    /// new run.
    fn place(&mut self, job: &crate::commit::CommitJob) -> JobAnswer {
        if let Some(run) = self
            .runs
            .get_mut(&job.id)
            .and_then(|runs| runs.last_mut())
            .filter(|r| r.live())
        {
            add_entries(run, &job.info.entries);
            return JobAnswer::Attached {
                run: run.name.clone(),
            };
        }
        if let Some(result) = self.cache.get(&job.id) {
            return JobAnswer::Cached {
                result: result.clone(),
            };
        }
        // A moved run keeps its laptop name, unless the id already has a run of
        // that name (the laptop's count restarted): then it is a `v<n>` too.
        let taken = |name: &String| {
            self.runs
                .get(&job.id)
                .is_some_and(|rs| rs.iter().any(|r| &r.name == name))
        };
        let name = if let Some(name) = job.info.run.clone().filter(|n| !taken(n)) {
            name
        } else {
            let n = self.numbered.entry(job.id.clone()).or_insert(0);
            *n += 1;
            format!("v{n}")
        };
        self.runs.entry(job.id.clone()).or_default().push(Run {
            name: name.clone(),
            phase: Phase::Queued,
            entries: job.info.entries.clone(),
            info: job.info.clone(),
            attempts: Vec::new(),
            last_attempt: 0,
            undone: 0,
            resume: false,
            // A moved run's laptop time counts in its duration.
            duration_secs: job.info.laptop_secs,
            lanes: Vec::new(),
            ended_at: None,
        });
        self.queue.push((job.id.clone(), name.clone()));
        JobAnswer::Queued { run: name }
    }

    /// Remove the entries under `order.key` from the old job's live run; cancel
    /// it if none is left and this commit does not attach to it. Returns the
    /// attempt to stop, if one runs.
    fn supersede(&mut self, order: &SupersedeOrder) -> Option<Stop> {
        let run = self
            .runs
            .get_mut(&order.old)?
            .iter_mut()
            .rev()
            .find(|r| r.live())?;
        run.entries.retain(|e| e.key != order.key);
        if !run.entries.is_empty() || order.attached {
            return None;
        }
        let name = run.name.clone();
        let marker = CancelMarker {
            by: order.label.clone(),
            replaced_by: Some(order.by.clone()),
        };
        let attempt = self.cancel_run(&order.old, &name, &marker)?;
        Some(Stop {
            id: order.old.clone(),
            run: name,
            attempt,
            marker,
        })
    }
}

/// Add `entries` to a run, each once.
fn add_entries(run: &mut Run, entries: &[Entry]) {
    for e in entries {
        if !run.entries.contains(e) {
            run.entries.push(e.clone());
        }
    }
}

/// What a commit's token records of a job's answer.
fn token_answer(a: &JobAnswer) -> TokenAnswer {
    match a {
        JobAnswer::Started { run } | JobAnswer::Queued { run } | JobAnswer::Attached { run } => {
            TokenAnswer::Reached { run: run.clone() }
        }
        JobAnswer::Cached { result } => TokenAnswer::Cached {
            result: result.clone(),
        },
        JobAnswer::Refused => TokenAnswer::NotApplied,
    }
}

/// The answer of a commit replayed after it applied: what its token recorded.
fn replay(jobs: &BTreeMap<String, TokenAnswer>) -> CommitReply {
    CommitReply::Applied {
        jobs: jobs
            .iter()
            .map(|(id, t)| {
                let a = match t {
                    TokenAnswer::Reached { run } => JobAnswer::Attached { run: run.clone() },
                    TokenAnswer::Cached { result } => JobAnswer::Cached {
                        result: result.clone(),
                    },
                    TokenAnswer::NotApplied => JobAnswer::Refused,
                };
                (id.clone(), a)
            })
            .collect(),
        dropped: Vec::new(),
        views: BTreeMap::new(),
    }
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;

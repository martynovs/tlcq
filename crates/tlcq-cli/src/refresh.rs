//! The refresh a preflight or a poll applies to the laptop's records and follows
//! (design D6, Preflight; the model's `Refreshed`).
//!
//! A record follows the id, so the laptop never runs a job the VM holds; a
//! follow follows its own run, resolved by token.

use crate::outcome::{JobResult, Outcome};
use crate::proto::{PreflightReply, TokenAnswer, VmState};
use crate::store::{Followed, JobRecord, RunRef, State};

/// What the refresh does to one record.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "one per record per refresh, used at once"
)]
pub enum Change {
    /// Nothing.
    Keep,
    /// It is on the VM: record `vm`, with the VM's run and entries as the mirror.
    Vm,
    /// It ended with this result.
    End(JobResult),
    /// A laptop job whose lock is free: the orphan rule applies (D7).
    Orphan,
}

/// The refresh of `record`, whose job lock is held or not, given the VM's reply
/// (`None` while the VM is unreachable).
pub fn refresh(record: &JobRecord, lock_held: bool, vm: Option<&PreflightReply>) -> Change {
    let unresolved = match record.state {
        State::Laptop if !lock_held => return Change::Orphan,
        State::Vm | State::Sent => true,
        State::Moving => !lock_held,
        State::Laptop | State::Ended => false,
    };
    let Some(vm) = vm.filter(|_| unresolved) else {
        return Change::Keep;
    };
    let not_applied = || {
        record
            .token
            .as_ref()
            .is_none_or(|t| vm.token(t, &record.id) == Some(TokenAnswer::NotApplied))
    };
    if record.state == State::Moving {
        // A move keeps its laptop run's name on the VM: only that run is the move's,
        // never an older run of the same id.
        let moved = same_run(record, vm);
        if not_applied() {
            // A failed move: its laptop run timed out, after the awake time it ran.
            return Change::End(JobResult {
                place: "laptop".to_owned(),
                duration_secs: record.elapsed_secs,
                ..ended(Outcome::Timeout)
            });
        }
        if !moved {
            return Change::Keep;
        }
    }
    match vm.state(&record.id) {
        // A run being cancelled: a send that applied nothing did not reach it,
        // unless it attached to that very run.
        VmState::Stopping
            if record.state == State::Sent && not_applied() && !same_run(record, vm) =>
        {
            Change::End(ended(Outcome::NeverSent))
        }
        VmState::Queued | VmState::Running | VmState::Stopping => Change::Vm,
        VmState::Cached { result } | VmState::Ended { result } => {
            // A send that applied nothing did not end an older run of its id: unless
            // it attached to that very run (the write-ahead keeps an attach's run, and
            // leaves a new job's empty), it never reached the VM.
            if record.state == State::Sent && not_applied() && !same_run(record, vm) {
                Change::End(ended(Outcome::NeverSent))
            } else {
                Change::End(result.clone())
            }
        }
        VmState::Unknown => match record.state {
            State::Sent if not_applied() => Change::End(ended(Outcome::NeverSent)),
            State::Vm => Change::End(ended(Outcome::Error {
                reason: Some("the VM does not know this job".to_owned()),
            })),
            _ => Change::Keep,
        },
    }
}

/// Whether the VM's latest run of the record's id is the run the record follows.
fn same_run(record: &JobRecord, vm: &PreflightReply) -> bool {
    vm.jobs.get(&record.id).is_some_and(|j| j.run == record.run)
}

/// What a follow of the job `id` holding `token` follows, once the VM has
/// answered for that token: one commit's token covers each of its jobs.
pub fn resolve_token(token: &str, id: &str, vm: &PreflightReply) -> Option<Followed> {
    match vm.token(token, id)? {
        TokenAnswer::Reached { run } => Some(Followed::Run(RunRef::Run(run))),
        TokenAnswer::Cached { result } => Some(Followed::Result(result)),
        TokenAnswer::NotApplied => Some(Followed::NeverSent),
    }
}

/// A result that is only an outcome, as the refresh learns it.
fn ended(outcome: Outcome) -> JobResult {
    JobResult {
        outcome,
        exit_status: None,
        distinct: None,
        generated: None,
        depth: None,
        place: "vm".to_owned(),
        duration_secs: 0,
        lanes: Vec::new(),
        errors: Vec::new(),
        note: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{CommitAnswer, VmJob};

    fn record(state: State, token: Option<&str>) -> JobRecord {
        let mut r = JobRecord::new("a", state);
        r.token = token.map(str::to_owned);
        r
    }

    fn reply(state: Option<VmState>, token: Option<TokenAnswer>) -> PreflightReply {
        let mut r = PreflightReply::default();
        if let Some(state) = state {
            r.jobs.insert(
                "a".to_owned(),
                VmJob {
                    state,
                    run: Some("v1".to_owned()),
                    entries: Vec::new(),
                    place: None,
                    elapsed_secs: None,
                    counters: None,
                    lanes: Vec::new(),
                    flags: crate::proto::Flags::default(),
                    curve: None,
                    position: None,
                    left_secs: None,
                    cancelled: None,
                    started_at: None,
                    expected: None,
                    waits_for: None,
                    held: None,
                },
            );
        }
        if let Some(t) = token {
            let commit = match t {
                TokenAnswer::NotApplied => CommitAnswer::NotApplied,
                answer => CommitAnswer::Applied {
                    jobs: [("a".to_owned(), answer)].into_iter().collect(),
                },
            };
            r.tokens.insert("t".to_owned(), commit);
        }
        r
    }

    fn ok() -> JobResult {
        ended(Outcome::Ok)
    }

    #[test]
    fn an_orphan_is_found_whatever_the_vm_says() {
        assert_eq!(
            refresh(&record(State::Laptop, None), false, None),
            Change::Orphan
        );
        assert_eq!(
            refresh(&record(State::Laptop, None), true, None),
            Change::Keep
        );
    }

    #[test]
    fn unresolved_records_stay_while_the_vm_is_unreachable() {
        for state in [State::Vm, State::Sent, State::Moving] {
            assert_eq!(
                refresh(&record(state, Some("t")), false, None),
                Change::Keep
            );
        }
    }

    #[test]
    fn a_live_vm_job_is_vm_and_an_ended_one_ends() {
        for live in [VmState::Queued, VmState::Running, VmState::Stopping] {
            assert_eq!(
                refresh(
                    &record(State::Sent, Some("t")),
                    false,
                    Some(&reply(Some(live), None))
                ),
                Change::Vm
            );
        }
        let cached = reply(Some(VmState::Cached { result: ok() }), None);
        assert_eq!(
            refresh(&record(State::Vm, None), false, Some(&cached)),
            Change::End(ok())
        );
        // A move that reached the VM as its laptop run's name, which then ended there.
        let mut done = reply(
            Some(VmState::Ended { result: ok() }),
            Some(TokenAnswer::Reached {
                run: "v1".to_owned(),
            }),
        );
        done.jobs.get_mut("a").unwrap().run = Some("l1".to_owned());
        let mut moving = record(State::Moving, Some("t"));
        moving.run = Some("l1".to_owned());
        assert_eq!(refresh(&moving, false, Some(&done)), Change::End(ok()));
    }

    #[test]
    fn unknown_to_the_vm() {
        let gone = reply(None, Some(TokenAnswer::NotApplied));
        assert_eq!(
            refresh(&record(State::Sent, Some("t")), false, Some(&gone)),
            Change::End(ended(Outcome::NeverSent))
        );
        let mut moving = record(State::Moving, Some("t"));
        moving.elapsed_secs = 300;
        assert_eq!(
            refresh(&moving, false, Some(&gone)),
            Change::End(JobResult {
                place: "laptop".to_owned(),
                duration_secs: 300,
                ..ended(Outcome::Timeout)
            })
        );
        assert!(matches!(
            refresh(&record(State::Vm, None), false, Some(&gone)),
            Change::End(JobResult {
                outcome: Outcome::Error { .. },
                ..
            })
        ));
        // A token the VM did not answer for is no proof: the record stays.
        let silent = reply(None, None);
        assert_eq!(
            refresh(&record(State::Sent, Some("t")), false, Some(&silent)),
            Change::Keep
        );
    }

    #[test]
    fn a_moving_job_with_its_supervisor_alive_is_left_alone() {
        let gone = reply(None, Some(TokenAnswer::NotApplied));
        assert_eq!(
            refresh(&record(State::Moving, Some("t")), true, Some(&gone)),
            Change::Keep
        );
        assert_eq!(
            refresh(&record(State::Ended, None), false, Some(&gone)),
            Change::Keep
        );
    }

    #[test]
    fn tokens_resolve() {
        let r = |t| reply(None, Some(t));
        assert_eq!(
            resolve_token(
                "t",
                "a",
                &r(TokenAnswer::Reached {
                    run: "v2".to_owned()
                })
            ),
            Some(Followed::Run(RunRef::Run("v2".to_owned())))
        );
        assert_eq!(
            resolve_token("t", "a", &r(TokenAnswer::Cached { result: ok() })),
            Some(Followed::Result(ok()))
        );
        assert_eq!(
            resolve_token("t", "a", &r(TokenAnswer::NotApplied)),
            Some(Followed::NeverSent)
        );
        assert_eq!(
            resolve_token("other", "a", &r(TokenAnswer::NotApplied)),
            None
        );
    }

    #[test]
    fn a_failed_move_is_not_an_older_run_of_its_id() {
        let mut moving = record(State::Moving, Some("t"));
        moving.run = Some("l1".to_owned());
        moving.elapsed_secs = 300;
        // v1, an older VM run of the same id, ended cancelled.
        let cancelled = ended(Outcome::Cancelled {
            by: "w2".to_owned(),
            replaced_by: Some("y".to_owned()),
        });
        let older = reply(
            Some(VmState::Ended { result: cancelled }),
            Some(TokenAnswer::NotApplied),
        );
        assert!(matches!(
            refresh(&moving, false, Some(&older)),
            Change::End(JobResult {
                outcome: Outcome::Timeout,
                duration_secs: 300,
                ..
            })
        ));
        // The move's token reached the VM, but the reply is still about v1: wait.
        let reached = reply(
            Some(VmState::Stopping),
            Some(TokenAnswer::Reached {
                run: "l1".to_owned(),
            }),
        );
        assert_eq!(refresh(&moving, false, Some(&reached)), Change::Keep);
        // The VM reports the moved run itself.
        let mut on_vm = reached.clone();
        on_vm.jobs.get_mut("a").unwrap().run = Some("l1".to_owned());
        on_vm.jobs.get_mut("a").unwrap().state = VmState::Queued;
        assert_eq!(refresh(&moving, false, Some(&on_vm)), Change::Vm);
    }

    #[test]
    fn a_send_that_never_arrived_is_not_an_older_run_of_its_id() {
        let timed_out = ended(Outcome::Timeout);
        let older = reply(
            Some(VmState::Ended {
                result: timed_out.clone(),
            }),
            Some(TokenAnswer::NotApplied),
        );
        // A new job: the write-ahead left its run empty.
        assert_eq!(
            refresh(&record(State::Sent, Some("t")), false, Some(&older)),
            Change::End(ended(Outcome::NeverSent))
        );
        // An attach to v1, which then ended: the record ends with v1's result.
        let mut attach = record(State::Sent, Some("t"));
        attach.run = Some("v1".to_owned());
        assert_eq!(
            refresh(&attach, false, Some(&older)),
            Change::End(timed_out)
        );
        // A live run of the id: the record follows the id, whatever the token says.
        let live = reply(Some(VmState::Running), Some(TokenAnswer::NotApplied));
        assert_eq!(
            refresh(&record(State::Sent, Some("t")), false, Some(&live)),
            Change::Vm
        );
    }

    #[test]
    fn a_send_that_never_arrived_does_not_follow_a_run_being_cancelled() {
        let stopping = reply(Some(VmState::Stopping), Some(TokenAnswer::NotApplied));
        assert_eq!(
            refresh(&record(State::Sent, Some("t")), false, Some(&stopping)),
            Change::End(ended(Outcome::NeverSent))
        );
        let mut attach = record(State::Sent, Some("t"));
        attach.run = Some("v1".to_owned());
        assert_eq!(refresh(&attach, false, Some(&stopping)), Change::Vm);
        assert_eq!(
            refresh(&record(State::Vm, None), false, Some(&stopping)),
            Change::Vm
        );
    }
}

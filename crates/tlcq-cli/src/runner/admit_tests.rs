//! Tests of admission: the slot rules and the round.

use super::*;
use crate::runner::state::tests::{run, running};

/// A launcher that records starts, freezes, thaws and saves, failing the
/// starts it is told to, and short of memory as told.
#[derive(Default)]
struct Fake {
    fail: BTreeSet<String>,
    no_memory: Vec<Slot>,
    /// No memory for a new attempt until a frozen container's limit is unpromised.
    full: bool,
    /// Freezes and thaws fail.
    stuck: bool,
    /// The memory free for a new attempt, over the slot's share.
    cap: Option<u64>,
    started: Vec<(String, u32, Slot)>,
    frozen: Vec<String>,
    thawed: Vec<(String, Slot)>,
    saves: usize,
    save_fails: bool,
}

impl Launch for Fake {
    /// A slot's 8 GB, the E slot's 16 GB alone, as `shares` says.
    fn memory(&mut self, slot: Slot, alone: bool, frozen: &BTreeSet<String>) -> Option<u64> {
        let s = self.shares();
        (!self.no_memory.contains(&slot) && (!self.full || !frozen.is_empty())).then_some(
            self.cap
                .unwrap_or(if alone { s.whole_mb } else { s.slot_mb }),
        )
    }

    fn shares(&self) -> Shares {
        Shares {
            slot_mb: 8192,
            whole_mb: 16_384,
        }
    }

    fn save(&mut self, _: &State) -> std::io::Result<()> {
        self.saves += 1;
        if self.save_fails {
            return Err(std::io::Error::other("disk full"));
        }
        Ok(())
    }

    fn start(&mut self, id: &str, _: &Run, attempt: &Attempt, _: u64) -> Result<(), String> {
        if self.fail.contains(id) {
            return Err("podman said no".to_owned());
        }
        self.started
            .push((id.to_owned(), attempt.number, attempt.slot));
        Ok(())
    }

    fn freeze(&mut self, id: &str, _: &str, _: u32) -> Result<(), String> {
        if self.stuck {
            return Err("podman said no".to_owned());
        }
        self.frozen.push(id.to_owned());
        Ok(())
    }

    fn thaw(&mut self, id: &str, _: &str, _: u32, slot: Slot) -> Result<(), String> {
        if self.stuck {
            return Err("podman said no".to_owned());
        }
        self.thawed.push((id.to_owned(), slot));
        Ok(())
    }
}

/// A state with these runs queued, in order.
fn queued(ids: &[&str]) -> State {
    let mut s = State::default();
    for id in ids {
        s.runs
            .insert((*id).to_owned(), vec![run("v1", Phase::Queued, "A.cfg")]);
        s.queue.push(((*id).to_owned(), "v1".to_owned()));
    }
    s
}

/// Run `id` given a live attempt 1 in `slot`, as `freeze`.
fn live(s: &mut State, id: &str, slot: Slot, freeze: Freeze) {
    let r = s.run_mut(id, "v1");
    r.phase = running();
    r.last_attempt = 1;
    r.attempts.push(Attempt {
        freeze,
        ..Attempt::new(1, slot, false)
    });
}

/// Run `id` paused, its attempt 1 ended for a pause, a resume queued if `resume`.
fn paused(s: &mut State, id: &str, resume: bool) {
    let r = s.run_mut(id, "v1");
    r.phase = running();
    r.last_attempt = 1;
    r.resume = resume;
    r.attempts.push(Attempt {
        ended: true,
        paused: true,
        ..Attempt::new(1, Slot::E, false)
    });
}

fn freeze_of(s: &State, id: &str) -> (Freeze, Slot) {
    let a = s.latest(id).unwrap().attempts.last().unwrap();
    (a.freeze, a.slot)
}

fn admit(s: &mut State, fake: &mut Fake) -> Vec<(String, String)> {
    s.admit(true, 1000, fake).unwrap()
}

fn started(s: &mut State) -> Vec<(String, u32, Slot)> {
    let mut fake = Fake::default();
    admit(s, &mut fake);
    fake.started
}

fn one(id: &str, slot: Slot) -> (String, u32, Slot) {
    (id.to_owned(), 1, slot)
}

#[test]
fn the_oldest_jobs_take_the_e_slot_then_the_p_slot() {
    let mut s = queued(&["a", "b", "c"]);
    assert_eq!(started(&mut s), [one("a", Slot::E), one("b", Slot::P)]);
    assert!(
        matches!(&s.latest("a").unwrap().phase, Phase::Running { place, .. } if place == "vm E")
    );
    assert_eq!(s.latest("c").unwrap().phase, Phase::Queued);
    assert_eq!(s.free_slots(true), []);
}

#[test]
fn with_the_p_slot_closed_jobs_run_one_at_a_time_in_e() {
    let mut s = queued(&["a", "b"]);
    let mut fake = Fake::default();
    s.admit(false, 1000, &mut fake).unwrap();
    assert_eq!(fake.started, [one("a", Slot::E)]);
    assert_eq!(s.free_slots(false), []);
    assert_eq!(s.free_slots(true), [Slot::P]);
}

#[test]
fn a_frozen_job_holds_no_slot_and_thaws_on_spare_capacity() {
    let mut s = queued(&["f", "a"]);
    live(&mut s, "f", Slot::E, Freeze::Frozen);
    // A waiting run comes first: a takes the slot f left, and P stays free
    // for nothing else.
    let mut fake = Fake::default();
    assert_eq!(
        admit(&mut s, &mut fake),
        [("a".to_owned(), "v1".to_owned())]
    );
    assert_eq!(fake.started, [one("a", Slot::E)]);
    // With nothing waiting, f thaws into P.
    assert_eq!(fake.thawed, [("f".to_owned(), Slot::P)]);
    assert_eq!(freeze_of(&s, "f"), (Freeze::Thawed, Slot::P));
    assert!(
        matches!(&s.latest("f").unwrap().phase, Phase::Running { place, .. } if place == "vm P")
    );
    assert_eq!(s.held_slots(), BTreeSet::from([Slot::E, Slot::P]));
}

#[test]
fn new_work_freezes_a_thawed_job_again_and_takes_its_slot() {
    let mut s = queued(&["a", "t", "n"]);
    live(&mut s, "a", Slot::E, Freeze::Running);
    live(&mut s, "t", Slot::P, Freeze::Thawed);
    let mut fake = Fake::default();
    admit(&mut s, &mut fake);
    assert_eq!(fake.frozen, ["t"]);
    assert_eq!(fake.started, [one("n", Slot::P)]);
    let t = s.latest("t").unwrap().attempts.last().unwrap();
    assert_eq!((t.freeze, t.frozen_at), (Freeze::Frozen, Some(1000)));
}

#[test]
fn a_re_freeze_waits_while_the_new_run_still_lacks_memory_and_spares_a_pausing_job() {
    let mut s = queued(&["a", "t", "n"]);
    live(&mut s, "a", Slot::E, Freeze::Running);
    live(&mut s, "t", Slot::P, Freeze::Thawed);
    let mut fake = Fake {
        no_memory: vec![Slot::P],
        ..Fake::default()
    };
    admit(&mut s, &mut fake);
    assert_eq!((fake.frozen.len(), s.short_of_memory), (0, true));
    // A pausing job is never frozen; nor one in a closed slot.
    s.run_mut("t", "v1").attempts[0].pausing = true;
    let mut fake = Fake::default();
    admit(&mut s, &mut fake);
    assert_eq!(fake.frozen.len(), 0);
    s.run_mut("t", "v1").attempts[0].pausing = false;
    s.admit(false, 1000, &mut fake).unwrap();
    assert_eq!(fake.frozen.len(), 0);
}

#[test]
fn freeing_a_frozen_job_s_memory_lets_the_new_run_start() {
    let mut s = queued(&["a", "t", "n"]);
    live(&mut s, "a", Slot::E, Freeze::Running);
    live(&mut s, "t", Slot::P, Freeze::Thawed);
    // Short of memory until t's unused limit is not promised.
    let mut fake = Fake {
        full: true,
        ..Fake::default()
    };
    admit(&mut s, &mut fake);
    assert_eq!(
        (fake.frozen.as_slice(), fake.started.len()),
        (["t".to_owned()].as_slice(), 1)
    );
}

#[test]
fn an_unfrozen_job_and_a_resume_go_before_the_queue() {
    let mut s = queued(&["a", "u", "r"]);
    live(&mut s, "u", Slot::P, Freeze::Unfrozen);
    paused(&mut s, "r", true);
    let mut fake = Fake::default();
    admit(&mut s, &mut fake);
    // In queue order among themselves: u thaws into E, r resumes in P, a waits.
    assert_eq!(fake.thawed, [("u".to_owned(), Slot::E)]);
    assert_eq!(freeze_of(&s, "u"), (Freeze::Running, Slot::E));
    assert_eq!(fake.started, [("r".to_owned(), 2, Slot::P)]);
    let r = s.latest("r").unwrap();
    assert!(!r.resume && r.attempts.last().unwrap().resume);
    assert_eq!(s.latest("a").unwrap().phase, Phase::Queued);
}

#[test]
fn a_paused_run_is_admitted_only_once_resumed() {
    let mut s = queued(&["p"]);
    paused(&mut s, "p", false);
    assert!(s.latest("p").unwrap().paused());
    assert_eq!(started(&mut s), []);
}

#[test]
fn a_thaw_needs_no_memory_and_a_stuck_container_ends_the_round() {
    // No memory for a new attempt: the unfrozen job and the frozen one thaw
    // all the same, their memory in use already.
    let new = || {
        let mut s = queued(&["u", "f"]);
        live(&mut s, "u", Slot::E, Freeze::Unfrozen);
        live(&mut s, "f", Slot::P, Freeze::Frozen);
        s
    };
    let mut s = new();
    let mut fake = Fake {
        no_memory: vec![Slot::E, Slot::P],
        ..Fake::default()
    };
    admit(&mut s, &mut fake);
    assert_eq!(
        fake.thawed,
        [("u".to_owned(), Slot::E), ("f".to_owned(), Slot::P)]
    );
    let mut s = new();
    let mut fake = Fake {
        stuck: true,
        ..Fake::default()
    };
    admit(&mut s, &mut fake);
    assert_eq!(freeze_of(&s, "u"), (Freeze::Unfrozen, Slot::E));
    // A run short of memory waits: a frozen job runs in the slot meanwhile.
    let mut s = queued(&["f", "a"]);
    live(&mut s, "f", Slot::P, Freeze::Frozen);
    let mut fake = Fake {
        no_memory: vec![Slot::E, Slot::P],
        ..Fake::default()
    };
    admit(&mut s, &mut fake);
    assert_eq!(fake.started, []);
    assert_eq!(
        (fake.thawed.as_slice(), s.short_of_memory),
        ([("f".to_owned(), Slot::E)].as_slice(), true)
    );
    assert_eq!(freeze_of(&s, "f"), (Freeze::Thawed, Slot::E));
    // A thawed job whose freeze fails keeps its slot.
    let mut s = queued(&["a", "t", "n"]);
    live(&mut s, "a", Slot::E, Freeze::Running);
    live(&mut s, "t", Slot::P, Freeze::Thawed);
    let mut fake = Fake {
        stuck: true,
        ..Fake::default()
    };
    admit(&mut s, &mut fake);
    assert_eq!(freeze_of(&s, "t"), (Freeze::Thawed, Slot::P));
}

#[test]
fn the_frozen_containers_are_the_frozen_and_unfrozen_ones() {
    let mut s = queued(&["a", "b", "c"]);
    live(&mut s, "a", Slot::E, Freeze::Frozen);
    live(&mut s, "b", Slot::P, Freeze::Unfrozen);
    live(&mut s, "c", Slot::P, Freeze::Thawed);
    assert_eq!(
        s.frozen_containers(),
        BTreeSet::from(["tlcq-a-v1-1".to_owned(), "tlcq-b-v1-1".to_owned()])
    );
}

#[test]
fn a_failed_start_undoes_its_record_and_ends_the_round() {
    let mut s = queued(&["a", "b"]);
    let mut fake = Fake {
        fail: BTreeSet::from(["a".to_owned()]),
        ..Fake::default()
    };
    assert_eq!(admit(&mut s, &mut fake), Vec::new());
    let a = s.latest("a").unwrap();
    assert_eq!(
        (a.attempts.len(), a.last_attempt, &a.phase),
        (0, 1, &Phase::Queued)
    );
    assert_eq!(fake.saves, 2);
    fake.fail.clear();
    admit(&mut s, &mut fake);
    assert_eq!(
        fake.started,
        [("a".to_owned(), 2, Slot::E), one("b", Slot::P)]
    );
}

#[test]
fn too_little_memory_ends_the_round_before_any_attempt_is_made() {
    let mut s = queued(&["a", "b"]);
    // The E slot's job fits, the P slot's does not: b waits, unspent.
    let mut fake = Fake {
        no_memory: vec![Slot::P],
        ..Fake::default()
    };
    assert_eq!(
        admit(&mut s, &mut fake),
        [("a".to_owned(), "v1".to_owned())]
    );
    let b = s.latest("b").unwrap();
    assert_eq!((b.last_attempt, &b.phase), (0, &Phase::Queued));
    assert!(s.short_of_memory);
    let saves = fake.saves;
    // Still short: nothing changes, nothing more is saved.
    admit(&mut s, &mut fake);
    assert_eq!(fake.saves, saves);
    fake.no_memory.clear();
    admit(&mut s, &mut fake);
    assert_eq!(fake.started.last(), Some(&one("b", Slot::P)));
    assert!(!s.short_of_memory);
}

#[test]
fn a_failed_resume_start_keeps_its_resume_queued() {
    let mut s = queued(&["r"]);
    paused(&mut s, "r", true);
    let mut fake = Fake {
        fail: BTreeSet::from(["r".to_owned()]),
        ..Fake::default()
    };
    admit(&mut s, &mut fake);
    let r = s.latest("r").unwrap();
    assert!(r.resume && r.attempts.len() == 1);
    assert!(matches!(r.phase, Phase::Running { .. }));
}

#[test]
fn a_state_that_cannot_be_saved_starts_nothing() {
    let mut s = queued(&["a"]);
    let mut fake = Fake {
        save_fails: true,
        ..Fake::default()
    };
    s.admit(true, 0, &mut fake).unwrap_err();
    assert_eq!(fake.started, Vec::new());
}

fn order(s: &State) -> Vec<&str> {
    s.queue.iter().map(|(i, _)| i.as_str()).collect()
}

#[test]
fn a_human_moves_queued_runs_only_among_queued_places() {
    let mut s = queued(&["a", "b", "c", "d"]);
    s.runs.get_mut("a").unwrap()[0].phase = running();
    assert!(s.reorder("c", "v1", Move::Up));
    assert_eq!(order(&s), ["a", "c", "b", "d"]);
    assert!(s.reorder("d", "v1", Move::Head));
    assert_eq!(order(&s), ["a", "d", "c", "b"]);
    assert!(s.reorder("d", "v1", Move::Down));
    assert_eq!(order(&s), ["a", "c", "d", "b"]);
    assert!(!s.reorder("c", "v1", Move::Up));
    assert!(!s.reorder("b", "v1", Move::Down));
    assert!(!s.reorder("a", "v1", Move::Head));
    assert!(!s.reorder("x", "v1", Move::Head));
    assert_eq!(order(&s), ["a", "c", "d", "b"]);
}

#[test]
fn an_unfrozen_job_goes_before_the_queue_and_a_thawed_one_is_frozen_for_it() {
    let mut s = queued(&["u", "a"]);
    live(&mut s, "u", Slot::E, Freeze::Unfrozen);
    let mut fake = Fake::default();
    admit(&mut s, &mut fake);
    assert_eq!(fake.thawed, [("u".to_owned(), Slot::E)]);
    assert_eq!(fake.started, [one("a", Slot::P)]);
    // Both slots busy, a thawed job in P: t is frozen, and u thaws into P.
    let mut s = queued(&["a", "t", "u"]);
    live(&mut s, "a", Slot::E, Freeze::Running);
    live(&mut s, "t", Slot::P, Freeze::Thawed);
    live(&mut s, "u", Slot::E, Freeze::Unfrozen);
    let mut fake = Fake::default();
    admit(&mut s, &mut fake);
    assert_eq!(
        (fake.frozen.as_slice(), fake.thawed.as_slice()),
        (
            ["t".to_owned()].as_slice(),
            [("u".to_owned(), Slot::P)].as_slice()
        )
    );
}

/// Run `id` asks for `mb` MB.
fn ask(s: &mut State, id: &str, mb: u64) {
    s.run_mut(id, "v1").info.memory_mb = Some(mb);
}

fn attempt_of(s: &State, id: &str) -> Attempt {
    s.latest(id).unwrap().attempts.last().unwrap().clone()
}

#[test]
fn a_request_one_slot_serves_starts_where_the_memory_reaches_it() {
    let mut s = queued(&["x", "a"]);
    live(&mut s, "x", Slot::E, Freeze::Running);
    ask(&mut s, "a", 6000);
    assert_eq!(started(&mut s), [one("a", Slot::P)]);
    let a = attempt_of(&s, "a");
    assert_eq!((a.memory_mb, a.whole), (Some(8192), false));
}

#[test]
fn a_run_short_of_its_request_waits_and_blocks_those_behind() {
    let mut s = queued(&["x", "f", "a", "b"]);
    live(&mut s, "x", Slot::E, Freeze::Running);
    live(&mut s, "f", Slot::E, Freeze::Frozen);
    ask(&mut s, "a", 8000);
    let mut fake = Fake {
        cap: Some(6000),
        ..Fake::default()
    };
    assert_eq!(admit(&mut s, &mut fake), []);
    assert!(s.short_of_memory);
    // P runs the frozen job meanwhile.
    assert_eq!(fake.thawed, [("f".to_owned(), Slot::P)]);
    assert_eq!(s.latest("b").unwrap().phase, Phase::Queued);
}

#[test]
fn a_whole_vm_run_starts_in_e_with_the_whole_vm_and_holds_p() {
    let mut s = queued(&["w", "b"]);
    ask(&mut s, "w", 12_000);
    assert_eq!(started(&mut s), [one("w", Slot::E)]);
    let w = attempt_of(&s, "w");
    assert_eq!((w.memory_mb, w.whole), (Some(16_384), true));
    // Nothing starts in P while it is live, not even on the next round.
    assert_eq!(started(&mut s), []);
    assert_eq!(s.latest("b").unwrap().phase, Phase::Queued);
}

#[test]
fn a_whole_vm_run_waits_for_p_and_p_starts_nothing_meanwhile() {
    let mut s = queued(&["y", "x", "w", "b"]);
    live(&mut s, "y", Slot::E, Freeze::Running);
    live(&mut s, "x", Slot::P, Freeze::Running);
    ask(&mut s, "w", 12_000);
    assert_eq!(started(&mut s), []);
    // x ends: P frees, but w is first and waits for E, so P starts nothing.
    s.run_mut("x", "v1").attempts[0].ended = true;
    assert_eq!(started(&mut s), []);
    assert_eq!(s.latest("b").unwrap().phase, Phase::Queued);
    // y ends: w takes E with the whole VM.
    s.run_mut("y", "v1").attempts[0].ended = true;
    assert_eq!(started(&mut s), [one("w", Slot::E)]);
}

#[test]
fn a_whole_vm_run_freezes_thawed_jobs_in_its_way_again() {
    // A thawed job in P: frozen again, then the run starts in E.
    let mut s = queued(&["t", "w"]);
    live(&mut s, "t", Slot::P, Freeze::Thawed);
    ask(&mut s, "w", 12_000);
    let mut fake = Fake::default();
    admit(&mut s, &mut fake);
    assert_eq!(fake.frozen, ["t"]);
    assert_eq!(fake.started, [one("w", Slot::E)]);
    // A thawed job in E with P empty: the same.
    let mut s = queued(&["t", "w"]);
    live(&mut s, "t", Slot::E, Freeze::Thawed);
    ask(&mut s, "w", 12_000);
    let mut fake = Fake::default();
    admit(&mut s, &mut fake);
    assert_eq!(fake.frozen, ["t"]);
    assert_eq!(fake.started, [one("w", Slot::E)]);
}

#[test]
fn a_frozen_whole_vm_run_keeps_p_held_and_a_paused_one_does_not() {
    let mut s = queued(&["w", "b", "c"]);
    live(&mut s, "w", Slot::E, Freeze::Frozen);
    s.run_mut("w", "v1").attempts[0].whole = true;
    // E frees for other work; P stays held.
    assert_eq!(started(&mut s), [one("b", Slot::E)]);
    assert_eq!(s.latest("c").unwrap().phase, Phase::Queued);
    let mut s = queued(&["w", "b", "c"]);
    paused(&mut s, "w", false);
    s.run_mut("w", "v1").attempts[0].whole = true;
    assert_eq!(started(&mut s), [one("b", Slot::E), one("c", Slot::P)]);
}

#[test]
fn a_job_a_human_froze_never_thaws_on_spare_capacity() {
    let mut s = queued(&["f"]);
    live(&mut s, "f", Slot::E, Freeze::Frozen);
    s.runs.get_mut("f").unwrap()[0].attempts[0].held = true;
    let mut fake = Fake::default();
    admit(&mut s, &mut fake);
    assert_eq!(fake.thawed, Vec::<(String, Slot)>::new());
    assert_eq!(freeze_of(&s, "f").0, Freeze::Frozen);
    assert!(s.held_slots().is_empty());
}

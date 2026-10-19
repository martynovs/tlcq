## 1. The model first

- [x] 1.1 `tla/runner/Runner.tla` (S7): slots `{"e", "p"}` with P closable; remove the restart, resize and too-big actions, the restart marker, the held slot and the queued restart (`HeldSlotSound` from every configuration, `RN_restartovercancel.cfg`; `RN_stalemarker.cfg` and `RN_stopbeforemark.cfg` re-targeted to the paused stop marker or removed); add freeze (slot freed, attempt live), thaw on spare capacity (the thawed state holds its slot), re-freeze for new work, unfreeze (first in the queue), pause (request, then the supervisor's stop marker at a checkpoint; a frozen attempt unpaused and stopped at once), resume (first in the queue), the P-slot toggle (a thawed job in P frozen again).
- [x] 1.2 New invariants: a frozen or paused run holds no slot, a thawed one exactly one; nothing starts or thaws in a closed slot; a paused run has a checkpoint; a pausing run is never frozen; one live attempt per run, frozen included; a paused run is still paused after a crash; `SlotsExclusive` over E and P. Model-check `RN`, `RN2`, `RN3` and every `RN_<bug>.cfg`; each race found gets an `RN_<bug>.cfg` that must fail. Update the module's header and design D14's paragraph on it.

## 2. Spike on pve

- [x] 2.1 On pve's podman, with nothing running (`tlcq ls`): `podman pause`, `podman update --cpuset-cpus` on the paused container, `podman unpause`, and `podman kill` of a paused container (and the same kill on Docker Desktop, for `just docker-up`); check that a tlx run continues on the new CPUs and that a killed paused container exits. Record the outcome in design S4 (the fallback: thaw only into the slot it froze in).

## 3. Pinned slots

- [x] 3.1 `Share` becomes `Slot` (`E`, `P`) in `runner/state.rs`; slot use derived from recorded attempts with no end, not frozen (S1, S2); frozen, thawed, pausing and paused as kinds of a live run (`Run::live`, preflight's older versions, attach, commit by id) and as a defaulted field of `VmState::Running` in `proto.rs`, so an older laptop reads them as running; a resumed or unfrozen run waiting first in the queue stays `Phase::Running`. Admission (`runner/admit.rs`): the oldest waiting run takes a free open slot, E first; no 100% share, no max rule; `now-or-refuse` against open slots.
- [x] 3.2 The CPU-list settings `TLCQ_E_CPUS` (default `16-31`) and `TLCQ_P_CPUS` (default `0-15`) in `cli.rs`, forwarded to attempts (`run.rs`); `Machine::resources` gives a slot's CPU count and its memory share (half, or all with P closed); `runner/docker.rs` starts containers with `--cpuset-cpus` instead of `--cpus`.
- [x] 3.3 The P-slot mark on the state volume (`runner/store.rs`), read by admission; closing it leaves a job running in P alone, and freezes a thawed one there unless it is pausing.

## 4. No lane restarts

- [x] 4.1 `lanes.rs`: `Act::Restart`, `Act::Resize` and `Act::TooBig` go; a big probe makes the job max in place in its slot; caps 2 h slow or unknown, 12 h max, in either slot (the side slot's cap goes). `runner/exec.rs` and `runner/settle.rs`: the restart and too-big markers, the held slot and the queued restart go; settling's outcome order loses them.
- [x] 4.2 Migration (design, Migration Plan): an attempt recorded with `full` or `main` reads as E, `side` as P; a queued restart reads as a queued resume.

## 5. Freeze

- [x] 5.1 The freeze and unfreeze UI verbs under the admission lock (S4): a freeze appended to the attempt's `frozen.log` with its time, `podman pause` through `Containers`, the frozen attempt left out of slot use; no freeze of a pausing job; unfreeze puts a frozen run first in the queue, or makes a thawed one running.
- [x] 5.2 Admission's spare-capacity thaw (oldest frozen job into a free open slot when nothing waits, a run short only of memory counting as waiting: `podman update --cpuset-cpus`, `podman unpause`, a thaw appended with its time and slot) and re-freeze (a waiting run with no free open slot freezes a thawed, not pausing job in an open slot and takes its slot, only if the run's memory then reaches `MIN_MB`); no thaw or unfreeze of a pausing job; `now-or-refuse` counts a thawed job's slot as free.
- [x] 5.3 Memory (`runner/memory.rs`): a frozen container's unused limit is not promised; a thaw or unfreeze waits until its limit is within the target slot's share and free memory leaves `MIN_MB` to grow; the page shows a frozen job memory keeps frozen and a run frozen jobs' memory holds.
- [x] 5.4 The supervisor leaves frozen time out of lane timers, caps, `--timeout` and the run's duration, from `frozen.log`'s intervals. A cancel of a frozen job kills its paused container; settling ends it `cancelled`.

## 6. Pause and resume

- [x] 6.1 The pause and resume UI verbs (S5): the `pause` request; the supervisor of a running or thawed job, just after the engine's next "Checkpointing completed" (today's resize handshake), writes its `paused` stop marker under the lock (replacing `restart`), stops the engine and keeps the checkpoint in the run's checkpoint directory; a frozen job is unpaused, its supervisor lets a checkpoint being written complete, then stops at once keeping the last completed checkpoint, or ends `cancelled` ("paused before its first checkpoint") with none; a request after the search ended is refused (the page offers none), one just before waits for the run's end and the page says so; a frozen job's pause appends its unpause to `frozen.log`; settling ranks the stop marker (never the request), only with the run's `checkpoint.json` kept (else `error`), and records the end "paused" with no outcome, and the run becomes paused (no slot, no queue place).
- [x] 6.2 Resume puts the run first in the queue; its next attempt recovers with `-recover` in whichever slot is free (tlx with the slot's workers; TLC with the checkpoint's workers and fingerprint function), keeps the run's lane and counts its timers and cap on from the earlier attempts' running time; a resumed attempt undone or failing to start returns to the head. A cancel of a paused run ends it `cancelled` and deletes its checkpoint.
- [x] 6.3 Both engines checkpoint every 10 minutes: `-checkpoint 10` in `tlx_args` and `tlc_args` (TLC passes none today, so it runs at its 30-minute default).

## 7. Reports and the page

- [x] 7.1 Places read `vm E` / `vm P`; `status`, `ls` and the submit report show frozen (since when, on spare capacity or not), pausing and paused (`status` with the checkpoint time and counters, as it alone gives a running job's counters); frozen, pausing and paused count as running for the exit code and for older versions (`plan.rs`).
- [x] 7.2 The page (`runner/ui.rs`, `ui/`): the slots card with E, P and whether P is open; Freeze / Unfreeze and Pause / Resume on runs as their state allows; the P-slot toggle; the queue-wide Pause renamed Hold queue / Release; frozen and paused runs listed with since when.

## 8. Deployment

- [x] 8.1 `docker/compose.yaml` passes `TLCQ_E_CPUS` and `TLCQ_P_CPUS` (pve's defaults); `just docker-up` sets lists that fit Docker Desktop's VM.
- [x] 8.2 The README's runner section: the two slots, the toggle, freeze and pause, and pausing max jobs before pve's nightly reboot.

## 9. Verification

- [x] 9.1 Tests for each piece above (admission, thaw and re-freeze, pause and resume with both engines' checkpoint layouts, frozen time, migration of old shares); `just clippy` with zero warnings, `just gate` green, `just ui-check`, `just fmt`, `just large-files`.
- [x] 9.2 Live on pve, with nothing else running: two jobs run pinned in E and P; a freeze frees a slot for a waiting job, and the frozen job thaws on spare capacity and is re-frozen by new work; a pause survives a runner restart and resumes in the other slot with its counts carried on.

## Context

The VM runner (`tlc-queue` D9, D10) has a 70% slot and a 30% side slot, sized as CPU time (`--cpus`), and a job alone takes both (100%). A max job moves between 70% and 100% through lane restarts: a restart marker holds the 70% slot, the stop hook queues the next attempt, which recovers the checkpoint the engine just wrote; a slow job judged big in the side slot with the 70% slot busy ends `error` (too big). The runner's TLA+ model (`tla/runner/Runner.tla`) checks exactly this protocol: restart markers, held slots, queued restarts, resizes.

pve's CPU is an i9-14900K: CPUs 0–15 are 8 performance cores with two threads each (up to 6.0 GHz), CPUs 16–31 are 16 efficiency cores (up to 4.4 GHz, one thread each). Measured with tlx (bec5985) on tlcq's `RN3`, distinct states in 5 minutes (`tmp/bench/`):

| Layout | Distinct states |
|---|---:|
| 1 job, 16 workers, unpinned | 11.99M |
| 2 jobs, 8 workers each, unpinned, side by side | 6.99M + 6.91M = 13.90M |
| 1 job, 16 workers, pinned to CPUs 0–15 | 9.92M |
| 1 job, 16 workers, pinned to CPUs 16–31 | 10.57M |
| 2 jobs, 16 workers each, pinned 0–15 and 16–31, side by side | 9.13M + 9.79M = 18.92M |

On four models tlx at 16 workers ran about 1.4× its 8-worker speed, and at 32 workers slower than at 16 (its workers wait at each level for the slowest, which sit on efficiency cores). Pinned by core type, two jobs barely slow each other (about 8%) and do 1.58× the work of one job on the whole machine.

pve reboots every night, so a run that must last longer than a day (or that is running at the reboot) needs to stop at a checkpoint and continue after it. The person who runs the queue orchestrates it from the runner's page; agents only use the CLI to submit and poll.

## Goals / Non-Goals

**Goals:**
- Two slots pinned by core type, equal, each with 16 workers; a mode with only the efficiency-core slot, so pve's other VMs get the performance cores.
- No lane restarts: simpler admission, a simpler runner model.
- From the page: freeze a running job (its slot freed at once, its memory kept), unfreeze it; pause a running job at a checkpoint (its slot and memory freed, surviving reboots), resume it.

**Non-Goals:**
- Resuming runs killed by a crash or an unplanned reboot (they still end `error`).
- Choosing a slot by job size, or sharing a slot between jobs.
- Freeze or pause from the agents' CLI.
- The laptop: unchanged.

## Decisions

### S1. Two pinned slots

The E slot holds CPUs `TLCQ_E_CPUS` (default `16-31`), the P slot `TLCQ_P_CPUS` (default `0-15`). An attempt's container gets `--cpuset-cpus <its slot's list>` instead of `--cpus`, and the engine `-workers <the list's CPU count>`: 16 for both defaults. Each open slot gets an equal share of the memory less the reserve (half with both open, all with one), still capped by what the VM has free when the attempt starts (`runner/memory.rs`, unchanged). tlx's `-maxmem` and TLC's heap and off-heap memory follow from the attempt's memory as before (tlx-engine E3).

*Alternatives:* 8-worker jobs, four at a time: two unpinned 8-worker jobs did only 1.16× one 16-worker job, as they shared the performance cores. A third "both" slot for a job alone (all 32 CPUs, 20% faster): dropped, since leaving it for the second job needs a resize at a checkpoint, the mechanism this change removes.

### S2. Admission

Admission stays first in, first out (with the page's reorder): the oldest waiting run takes a free open slot, the E slot first. There is no 100% share and no rule for max jobs: two max jobs may hold both slots, and the page's freeze or pause frees one (S4, S5). `now-or-refuse` asks whether a slot is free now, as before. The slot an attempt holds is recorded on it (`e` or `p`), as its share is today.

The single-slot toggle (S6) closes the P slot: admission puts nothing new there; a job already running in it keeps running.

### S3. No lane restarts

The lanes keep their timers, probe and caps, and lose every restart: a short job still running at 15 minutes becomes slow; a slow or unknown job judged big at its probe becomes max in place in whichever slot it holds, with max's cap. Gone: the restart at 70%, growth to 100%, the resize at a checkpoint, the restart marker, the held slot, the queued restart, the too-big marker and its `error` outcome, the side slot's 3-hour cap. Caps: 2 hours slow or unknown, 12 hours max, `--timeout` as before. `Act::Restart`, `Act::Resize` and `Act::TooBig` go from `lanes.rs`; `Share::Full`/`Main`/`Side` become `Slot::E`/`P`.

### S4. Freeze

From the page, on a running job that is not pausing, under the admission lock: the runner appends a freeze (with the time) to the attempt's `frozen.log` and runs `podman pause` on its container, which stops the engine and its supervisor in place (no CPU, memory kept). A VM attempt is then in one of three states, read from the last line of that log: **running**, **frozen** (holds no slot) or **thawed** (on spare capacity: holds its slot, but can be frozen again for new work). Slot use, derived as today from recorded attempts with no end, leaves out frozen ones only. A frozen attempt is still its run's one live attempt.

Memory: a frozen job keeps what it uses, which admission's memory cap already sees (it reads what the VM has free); the rest of its container's limit is not promised while it is frozen, so the job started in its slot gets the memory the VM really has free. A frozen job thaws (either way below) when its limit is within the share of the slot it thaws into, and the VM's free memory, less the reserve and the room the running attempts may still grow into, is at least `MIN_MB`, the least a new attempt starts with. Not its whole unused limit: both slots start with half, and the host's own use is already out of what is free, so that would never fit while the other slot runs (found live on pve: about 300 MB short). Every attempt's limit is at most its slot's share, so E's and P's limits together stay within the memory less the reserve; the free-memory check guards against outside users such as VM 700. Until it fits the job stays frozen, and the page says so. A run that waits only for memory still counts as waiting, so no spare-capacity thaw takes memory it is waiting for; the page shows that frozen jobs hold the memory, and the human pauses one.

A frozen job runs again in one of two ways:
- **On spare capacity.** When an open slot is free and no other run waits for one, admission thaws the oldest frozen job into it: under the lock, `podman update --cpuset-cpus <that slot's list>` (the slot may differ from the one it froze in; both have 16 CPUs, so the engine's 16 workers carry on), `podman unpause`, and a thaw appended to `frozen.log` with the time and the slot. When a run waits, no open slot is free and a thawed job holds an open one, admission freezes it again (instantly) and starts the run there, but only if that run's memory, with the frozen job's unused limit no longer promised, reaches `MIN_MB` (else nothing is frozen for it). Closing the P slot freezes a thawed job in it at once, unless it is pausing (it keeps P until its pause lands); nothing thaws into a closed slot. For `now-or-refuse`, a slot held by a thawed job counts as free.
- **Unfreeze.** From the page: the job becomes an ordinary run again. A thawed job becomes running in its slot; a frozen one goes first in the queue and takes the next free slot.

A pausing job (S5) is never frozen, thawed or unfrozen, by the page or by admission: it leaves its slot within minutes anyway, and a frozen one being paused is unpaused by its pause, which appends that to `frozen.log`.

Time while frozen does not count: the supervisor reads the intervals from `frozen.log` (an append-only log, so a quick thaw and re-freeze lose none) and leaves frozen time out of its lane timers, its caps, `--timeout` and the run's duration, as the laptop leaves out sleep. The progress curve already reads a gap in the engine's clock as a sleep (`curve.rs`'s slack), so the estimates stay sane.

A cancel or supersede of a frozen job kills its paused container (podman and Docker Desktop both kill a paused container), and settling ends it `cancelled`. A runner restart leaves a paused container running-but-paused, and the liveness query already counts a paused container as live (`runner/docker.rs`). A reboot kills a frozen job like a running one: `error`.

Frozen, thawed, pausing and paused are kinds of a live run, not new ends, and so is a resumed or unfrozen run waiting first in the queue (it stays `Phase::Running` while it waits, as today's queued restart does, so an agent's resubmit is refused rather than superseding it): preflight, attach, older versions and commit by id treat them as running (`Run::live`), and the wire format carries them as a field of the running state with a default, so a laptop that predates them still reads "running".

*Alternatives:* no automatic thaw (a forgotten frozen job wastes a free slot); thaw ahead of the queue (a frozen job would never yield).

### S5. Pause and resume

From the page, on a running or frozen job:
- **Pause** writes a `pause` request in the attempt's directory. A running or thawed job's supervisor waits for the engine's next completed checkpoint, then, under the lock, writes its own `paused` stop marker and stops the engine (the handshake of today's resize, whose `restart` marker it replaces). The checkpoint moves to the run's checkpoint directory, `<state>/checkpoints/<id>/<run>/`, as a resize keeps it today, and settling, which ranks the supervisor's stop marker and never the request, records the end "paused" with no outcome, but only when the run's checkpoint was kept (`checkpoint.json`); a container gone between the marker and the move ends `error`. A container gone after the request but before the stop marker (out of memory, a reboot) ends as it would without the request: `error`.
- **A frozen job** is unpaused in place for its pause: its supervisor sees the request, lets a checkpoint already being written complete, and stops the engine at once, keeping the last checkpoint it saw completed (it runs for seconds on the CPUs it froze on, possibly beside another slot's job). One that never completed a checkpoint ends `cancelled` with "paused before its first checkpoint", and says so.
- **After the search.** Neither engine checkpoints once its search has ended and its liveness check runs (tlx always, TLC's max jobs with `-lncheck final`), so a pause is refused from then on (tlx has also deleted its checkpoint by then), and the page offers none. A pause requested just before waits for the run's end, and the page says no checkpoint is coming. A frozen job is refused too: unpaused for a pause, it would run its liveness check in no slot, beside another slot's job.
- **A paused run** holds no slot and no memory, is in no slot and not in the queue's admission order, and is all files on the state volume, so it survives reboots. `status` and the page show it `paused`; it counts as running for older versions (an agent's `--replace` cancels it) and does not make `status` exit non-zero.
- **Resume** puts it first in the queue. Its next attempt recovers the checkpoint with `-recover` in whichever slot is free: tlx with that slot's workers, TLC with the checkpoint's (16, the same in both slots); its curve carries the run's counts as a recovered attempt's does today (tlx-engine E4). It keeps the run's lane, and its lane timers and cap count on from the earlier attempts' running time, so a pause never buys a fresh 12 hours.
- **Cancel** of a paused run ends it `cancelled` and deletes its checkpoint.

Checkpoints are therefore kept: both engines checkpoint every **10 minutes** (`-checkpoint 10`), so a pause lands within about 10 minutes plus the current search level, and the person running the queue pauses max jobs that long before the nightly reboot. A checkpoint is deleted when its run ends. Nothing resumes a run killed by a crash or an unplanned reboot: it ends `error`, as today.

### S6. The page

- Each row's job names its slot (`vm E`, `vm P`) instead of a share; the slots card shows E and P, and whether P is open.
- Buttons: **Freeze** / **Unfreeze** and **Pause** / **Resume** on a run, as its state allows; the single-slot toggle (**P cores: open / closed**), stored as a mark on the state volume (survives restarts and reboots).
- The existing queue-wide **Pause** (the stopping mark that holds admission) is renamed **Hold queue** / **Release**, so "pause" names only a run's pause.
- Each is a UI verb under the admission lock, like cancel and reorder.

### S7. The runner model first

`tla/runner/Runner.tla` changes before the code: the slots become `{"e", "p"}`, with P closable; the restart, resize and too-big actions, the lanes, the held slot and the hook's queue step go (with `HeldSlotSound`, `RN_stalemarker`, `RN_endbeforequeue` and `RN_unkeyedqueue`, races of the restart protocol only); freeze, thaw (spare capacity, unfreeze), re-freeze, the page's pause request, the supervisor's paused stop marker and the checkpoint's move after it, a frozen job's pause (unpaused, stopped at once, or `cancelled` with no checkpoint), resume (a queued next attempt, as a restart was) and the P toggle come in, the page's actions bounded by `MaxUi`. Invariants added: a thawed job holds its slot (`SlotsExclusive` over running and thawed attempts); nothing starts or thaws in a closed P slot (`ClosedSlot`); a pausing job is never frozen (`PausingNotFrozen`); a paused attempt's checkpoint was kept (`PausedHasCheckpoint`); a cancelled run is not paused (`CancelFinal`); a paused run stays paused, crashes included, until a resume or cancel (`PausedStays`). Bug variants moved or added: `RN_stopbeforemark` (the stop before the paused marker), `RN_pausedovercancel` (was `RN_restartovercancel`), `RN_resettleended` (a cancelled paused run settled paused again), `RN_pausebyrequest`, `RN_nockcheck`, `RN_thawedfree`. Each new race the reviews or TLC find gets an `RN_<bug>.cfg` that must fail.

## Risks / Trade-offs

- [The CPU lists are pve's] → settings with pve's defaults; `just docker-up` sets lists that fit Docker Desktop's small VM; a list naming a CPU the host lacks makes the container start fail, which leaves the job queued and is visible on the page.
- [Two max jobs hold both slots for up to 12 h] → freeze or pause one from the page; the person running the queue decides.
- [A frozen job holds its memory] → admission's memory cap already sees what it uses, and its unused limit is not promised while frozen; a frozen job thaws only within its slot's share and with room left to grow; the human pauses it to free the memory.
- [A pause pending at a reboot loses the run] → the page shows "pausing" until it lands, and the person pauses early; a pause requested after the search (liveness check) never lands before the run ends.
- [A pause waits up to 10 minutes plus a level; a tlx level may take longer on big models] → pause well before the reboot; the page shows "pausing" until it lands.
- [A frozen job is lost at a reboot] → pause instead of freeze for anything that must survive the night.
- [Moving a frozen job between slots] → checked on pve (podman 5.4.2, 2026-10-19): a tlx run on CPUs 16–31 paused, `podman update --cpuset-cpus 0-15` while paused, unpaused: all 21 threads on 0–15, about 12 cores busy, and its counts went on (1.2M to 2.7M distinct in a minute).
- [10-minute checkpoints cost disk writes on big jobs] → tlx writes them incrementally (seconds each); TLC's are full writes: measured on the first big TLC job.
- [The runner model grows in one place while shrinking in another] → the model changes first, and the code follows it.

## Migration Plan

1. Deploy when nothing runs on the VM (`tlcq ls`), as for any runner change (a deploy builds on pve itself), and `just install` the laptop with it; a laptop that predates the change reads frozen and paused runs as running.
2. A run queued before the deploy keeps its place; the new runner reads an attempt's old share as its slot (`main` and `full` as E, `side` as P).
3. A queued restart from before the deploy reads as a queued resume (first in the queue, recovering a checkpoint if one was kept); a held slot is gone with the restart marker, which nothing reads any more.
4. Rollback: redeploy the previous image when nothing runs; a frozen or paused run must be resumed or cancelled first, since the previous runner knows neither state.

## Open Questions

- Whether TLC's 10-minute checkpoints are too slow on its biggest jobs; the interval could differ per engine.
- Whether the E slot stays the better one for TLC, which the benchmarks did not measure pinned.

## Why

The VM's two slots split its CPU time 70/30 (`--cpus`), as if its 32 CPUs were alike. pve's CPU is an i9-14900K: CPUs 0–15 are its 8 performance cores with two threads each, CPUs 16–31 its 16 efficiency cores, so a job's workers drift between fast and slow cores, and tlx's level-synchronous workers wait at every level for the slowest. Measured on pve (tlx at bec5985, tlcq's `RN3`, distinct states reached in 5 minutes): one 16-worker job unpinned reached 11.99M; two 8-worker jobs side by side, unpinned, 13.90M together (1.16×); two 16-worker jobs pinned one to each core type, 9.13M + 9.79M = 18.92M (1.58×), each only about 8% slower than alone. Pinned by core type, the two halves of the CPU are worth about the same, and two jobs barely slow each other.

With two equal slots there is nothing to grow into or shrink from, so the lane restarts that resize a max job between 70% and 100% can go. What the person running the queue needs instead is to step in from the runner's page: free a slot at once for other work, and stop a long job at a checkpoint before pve's nightly reboot and continue it the next day.

## What Changes

- **BREAKING** The VM's slots become two equal slots pinned to CPU lists: the E slot (default CPUs 16–31, the efficiency cores) and the P slot (default CPUs 0–15, the performance cores' threads). A container gets `--cpuset-cpus` with its slot's list instead of `--cpus`, and `-workers` equal to the list's CPU count (16); each open slot gets an equal share of the memory less the reserve, still bounded by what the VM has free. The lists are settings.
- A single-slot toggle on the runner's page closes the P slot, so tlcq runs on the efficiency cores only and pve's other VMs get the performance cores; it survives restarts and reboots.
- Admission stays first in, first out, the E slot first. There is no 100% share and no limit on max jobs.
- **BREAKING** Lane restarts go: no restart at 70% or growth to 100%, no resize, no restart marker, held slot or queued restart, no too-big stop. A job judged big becomes max in place. Caps are 2 hours slow or unknown and 12 hours max in either slot.
- **Freeze / Unfreeze** on the page: a frozen job's container is paused (no CPU, memory kept) and its slot freed at once. It runs again on spare capacity (a free slot with nothing waiting) until new work needs that slot, or as an ordinary run once unfrozen. Frozen time counts toward no timer or cap.
- **Pause / Resume** on the page: a paused job stops at its next checkpoint and holds no slot or memory, surviving reboots; resumed, it goes first in the queue and recovers the checkpoint in either slot. Both engines checkpoint every 10 minutes.
- The page's queue-wide "Pause" (which holds admission) is renamed "Hold queue".
- Agents keep the CLI as it is; `status` shows frozen and paused jobs, which count as running.
- The runner's TLA+ model (`tla/runner`) changes first and is model-checked again.
- The laptop is unchanged.

## Capabilities

### New Capabilities
(none)

### Modified Capabilities
These specs are still in the unarchived `tlc-queue` and `tlx-engine` changes; their deltas apply once those are archived, in that order.
- `queue-runner`: two pinned slots and the single-slot mode; admission without the 100% share; lane decisions without restarts; freeze and pause in the attempt lifecycle; checkpoints kept only for pause.
- `runner-ui`: the page shows the two slots; freeze, unfreeze, pause, resume and the single-slot toggle; "Hold queue".
- `checker-engine`: both engines checkpoint every 10 minutes; recovery serves resume instead of a lane restart.
- `job-submit`: an older version counts as running while frozen or paused instead of while a restart is pending.
- `job-status`: reports name the slot instead of a share and show frozen and paused.

## Impact

- Code: `runner/state.rs` (`Share` becomes the slot, frozen and paused states, slot use), `runner/admit.rs` (admission, spare-capacity thaw and re-freeze), `lanes.rs` (`Act::Restart`, `Resize`, `TooBig` go; pause at a checkpoint), `runner/exec.rs` (markers, frozen time, pause handshake, arguments), `runner/settle.rs`, `runner/store.rs` (checkpoints kept for pause, the single-slot and hold marks), `runner/docker.rs` (`--cpuset-cpus`, pause, unpause, update), `runner/memory.rs` (equal shares; a frozen job's unused limit not promised), `runner/verbs.rs` (the `Containers` trait, the settle sweep, the new UI verbs), `proto.rs` (frozen, thawed, pausing and paused as kinds of running, readable by an older laptop), `refresh.rs`, `snapshot.rs`, `runner/ui.rs` and `ui/` (buttons, slot names, toggle), `run.rs` and `cli.rs` (the CPU-list settings, forwarded to attempts), reports and `status`.
- The TLA+ model `tla/runner` and its configurations.
- Deployment: compose and `just docker-up` set CPU lists that fit Docker Desktop's small VM; pve uses the defaults.

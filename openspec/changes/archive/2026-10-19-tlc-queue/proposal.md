## Why

TLA+ models are checked before they are committed, and the big configurations take from an hour to many hours. On a laptop they compete with builds and with each other, and they get killed by time caps before they finish. A dedicated TLC machine with one queue, which any agent in any project or worktree can submit to, runs them as fast as it can, bounded only by generous time caps, and keeps the laptop free.

## What Changes

- A `tlcq` binary (Rust) with two roles: a client on the laptop, and a runner in a long-lived container on the home server `pve` (rootless podman, reached through the `pve` docker context), which holds the queue and runs each job in a container of its own. "The VM" below means that side.
- One interface: an agent calls `tlcq submit <dir> [cfg…]`, and tlcq decides where each job runs. The agent never names a machine, except to force a long job onto the laptop with `--local` when the VM is down.
- A job is the `.tla` modules of one model directory (plus the directories its spec EXTENDS) and one configuration, sent as a tarball on the runner command's stdin. Nothing about the job travels on the command line except fixed verbs and ids.
- A job's id is a content hash of what it checks, so an unchanged model returns its stored result at once. The laptop caches its own results and every VM result it sees; the VM caches its own.
- Placement by expected duration: a flag (`--fast`, `--slow`, `--max`) or a configuration name suffix (`_fast`, `_slow`, `_max`) when there is one, else the durations recorded for earlier runs. With no configuration named, max ones are skipped and listed unless `--all` is given. Long jobs run on the VM. Short ones run on the VM when it has room now, else on the laptop when its one slot is free, else wait on the VM. A job with no history runs on the laptop when its slot is free, capped at 5 minutes, else on the VM; a timeout is recorded, so the next run goes to the VM. Nothing is renamed.
- The VM runs at most two jobs with no resource caps, in a 70% slot and a 30% side slot: first in, first out: on an idle VM the oldest waiting job takes both if it is not max, even with others waiting, and a max job only ever takes the 70% slot, leaving the side slot for others. The runner admits jobs when one arrives and when one ends, even while the laptop sleeps, runs each attempt in its own container, and decides pass or fail from the configuration's header (`must fail <Property>`).
- Lanes move up one step at a time, decided on the VM by each job's supervisor: a short job still running at 15 minutes becomes slow; a slow one that `tla/probe.sh`'s size probe judges big 15 minutes into the slow lane becomes max (restarted at 70% if it ran at 100% and a non-max job waits, else max in place at 100%; restarted at 70% if it ran in the side slot with the 70% slot free; max in place if it ran in the 70% slot; else stopped as too big). Time caps end a job as `timeout`, so none holds the VM forever: 2 hours for slow or unknown jobs (3 in the side slot), 12 hours for max ones; `--timeout` overrides.
- The agent submits, gets a report at once (id, machine, running or its queue position), and polls `tlcq status`, which covers its current jobs (the latest per configuration, and any older one still queued or running for it) in one call: full progress (counters, the probe's numbers, time left) for running jobs, queue position for waiting ones, the result for ended ones.
- A web UI on the runner, behind pve's traefik (`tlcq.pve.local`), for a human: the slots, the queue, each job's progress and ETA, results and logs; it can cancel a job and reorder the queue.

## Capabilities

### New Capabilities
- `job-submit`: packaging a model and a configuration into a job, its content-hash id, the duration history, placement between the laptop and the VM, and runs on the laptop.
- `queue-runner`: the runner and its queue, its two slots and time caps, one container per attempt, the verdict, and cancel.
- `runner-ui`: a web page on the runner for a human to watch the queue, cancel jobs and reorder the queue.
- `job-status`: progress, waiting, results and their outcomes, and the caches.

### Modified Capabilities
(none: a new project)

## Impact

- New repository `tlcq`; a job image (Java, the pinned TLC jars, the `tlcq` binary for Linux) built from it; on pve, the runner as a compose service with a state volume, the podman socket, `restart: always` and traefik labels for its UI; the same pinned jars on the laptop.
- A submission is described in TOML (the spec module, the directories its spec EXTENDS, the configurations and their options), from a `tlcq.toml` in the model directory, from a file, or piped on stdin; a project's existing `tla/tlc.sh` table becomes one `tlcq.toml` per directory. Existing configuration names keep working.
- Later (not this change): a pre-commit gate that refuses a commit whose model text has no matching passing result in the laptop's cache.

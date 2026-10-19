# job-status Specification

## Purpose
TBD - created by archiving change tlc-queue. Update Purpose after archive.
## Requirements
### Requirement: The submit report
`tlcq submit` SHALL reply at once with a report per job: its id, its configuration (every name, for configurations sharing one id), where it went (laptop, or VM and its slot, E or P), and its state: running, queued (its position and how many jobs are ahead of it), a cached result, attached to an existing run, or refused (and why); plus its expected duration and the source of that expectation, and its time cap.

#### Scenario: A batch
- **WHEN** an agent submits a directory whose jobs go to the VM, the laptop and the cache
- **THEN** the report gives each job's id and machine, and says which run now, which wait (with their queue position) and which were cached

### Requirement: Polling the agent's jobs
`tlcq status` SHALL say first when the VM's free disk is below twice its floor (`the VM's disk is low: N GB free, no job starts below F GB`). With no ids it SHALL report the calling agent's current jobs from a follow the laptop keeps per entry the agent places (found by worktree path and `$TLCQ_AGENT`, unset matching unset, never by branch; the key: worktree, model directory, spec module, configuration name; the id; a placement number fixed at creation; what it follows: a run, a cache hit's result, or a sent job's token, which the command's answer, or else a later preflight or poll, rewrites to what it reached, or to `never sent` once a lookup has made its token final (the VM then never applies that commit)), which no mirror refresh or placement anew of the id changes: for each key the one with the highest placement number, cache hits included, plus any older one whose run is still queued or running with its key's entry (one still holding a token while the VM is unreachable counts as running); an older one ended, cancelled (its container still stopping included) or superseded out of its key's entry drops out, and a placement matching a follow the agent already has (same id, run and key) adds none; `tlcq status <id>…` SHALL report the named ones, each by the agent's follow of that id with the highest placement number, else the id's latest run; `tlcq ls` SHALL report every job. Per job, the report SHALL give, when running: its place, slot and any move, its lane and the source of it (and any lane decision made, D9), its time cap and time left, its start time, its duration, its estimated end time and the source of the estimate (the latest full run of the same spec and configuration text (D5's exact key) or of the same configuration text with an earlier spec, an average of a run under the same configuration name and a first-run estimate, a first-run estimate, or none before 5 minutes into the current attempt), TLC's latest counters (states generated, distinct, left on the queue, and their rates; for tlx, whose level-by-level search counts only the level being expanded, so its queue jumps by the next level at each boundary, the queue as TLC counts it: what is left of the level plus the states found since it started, and beside it what is left of the level with its change per minute, as the level drains while the queue may grow), the queue as a fraction of the distinct states, and the queue's growth over the last 5 minutes as a fraction of the distinct states; when queued: its position, how many jobs are ahead of it, and what holds it (a VM slot, disk, the laptop's slot); when cancelled with its container still stopping: stopping, with the canceller's label and any "replaced by" id, counted as `cancelled` for the exit code; when frozen: frozen, since when, and whether it runs on spare capacity now; when pausing: pausing, until its next checkpoint; when paused: paused, with when it stopped at its checkpoint and its counters then, and, when it died (a long run, or a resume), why (with the memory to ask for, up to the most one job gets, when killed for memory), the disk its kept checkpoint holds, and that `tlcq resume` (with `--memory` for more) recovers it and `tlcq cancel` drops it; when resumed or unfrozen and waiting first in the queue: waiting for a VM slot, with no slot named and no estimated end; a frozen job not on spare capacity, a paused or a waiting job SHALL get no estimated end; a frozen, pausing or paused job SHALL count as running for the exit code; when ended: its outcome (with the canceller's label and any "replaced by" id, or the reason for an `error` such as never started, and for one killed for memory the memory to ask for, up to the most one job gets), TLC's exit status, distinct and generated states, depth, place and duration. A report SHALL follow the run its agent placed or attached to, not the id: when that run has ended, the report gives its outcome, even if the id has since been placed anew by someone else. `status` SHALL exit 0 unless a job it reports failed: 3 when any ended `timeout`, `cancelled`, `error` or `never sent`, else 1 when any is `unexpected`; a job still queued or running SHALL NOT make it exit non-zero. An id that neither the laptop nor the VM knows SHALL be reported `error`; while the VM is unreachable, a job on it SHALL be reported with its last known state, marked as such. After the jobs, `status` SHALL list the checkpoints the reported paused runs keep on the VM's disk, each with its size, and their total, saying `tlcq cancel <id>` deletes one. `tlcq resume <id> [--memory <size>] [--timeout <duration>]` SHALL resume the paused VM run the agent follows from its kept checkpoint, first in the queue, its attempts asking for that memory and capped at that duration when given; it SHALL be refused, exiting 2, past the most one job gets, at or below the time the run has run, or when the run is not paused.

#### Scenario: A long run
- **WHEN** an agent polls a max job that has run for three hours
- **THEN** `tlcq status` shows its counters from TLC's latest progress line, the queue's growth and the time left before its cap, and exits 0, so the agent can decide to keep or cancel it

#### Scenario: An ETA from a previous run
- **WHEN** an agent polls a job after a spec edit, whose configuration ran fully under the earlier spec, and the new run goes at half the earlier speed
- **THEN** the estimated end is scaled to the current size and speed, not copied from the earlier duration, and its source is that earlier run

#### Scenario: Ended older versions drop out
- **WHEN** an agent's earlier version of a configuration ended `unexpected` and its new version runs
- **THEN** `tlcq status` with no ids reports only the new version, and exits 0

#### Scenario: Replacing one's own running version
- **WHEN** an agent submits a new version with `--replace`, and polls while the older version's container is still stopping
- **THEN** `status` with no ids reports only the new version, and exits 0 unless the new one failed

#### Scenario: Replacing a shared version
- **WHEN** an agent submits a new version with `--replace` while another worktree also holds the older one, which keeps running for it
- **THEN** `status` with no ids reports only the new version

#### Scenario: Both kinds of failure
- **WHEN** one of the agent's jobs is `unexpected` and another ended `timeout`
- **THEN** `status` exits 3

#### Scenario: An ETA on a first run
- **WHEN** an agent polls a job with no history 15 minutes in
- **THEN** the report gives an estimate from the job's own progress curve, marked as a first-run estimate

#### Scenario: Too early for an ETA
- **WHEN** an agent polls a job 3 minutes into its current attempt
- **THEN** the report gives no estimated end yet

#### Scenario: A queued job
- **WHEN** an agent polls a job waiting behind two others for a VM slot
- **THEN** the report gives its position, the two jobs ahead and that a slot holds it, and exits 0

#### Scenario: All my jobs
- **WHEN** an agent runs `tlcq status` with no ids
- **THEN** it sees the latest job for each configuration it submitted or attached to, and any of its jobs still queued or running, and no other agent's

#### Scenario: A failure
- **WHEN** one of the agent's jobs ended `unexpected` and the rest still run
- **THEN** `status` reports all of them and exits 1

#### Scenario: A run ends and the id runs again
- **WHEN** an agent's laptop run dies, and another worktree places the same id anew before the agent polls
- **THEN** the agent's report gives `error` for its own run, not the new run's state

#### Scenario: An unknown id
- **WHEN** an agent polls an id that neither machine knows
- **THEN** `status` reports `error` for it and exits 3

#### Scenario: The VM unreachable
- **WHEN** an agent polls while the VM is unreachable
- **THEN** its VM jobs are reported with their last known state, marked unreachable, and `status` exits 0 unless a reported job failed

#### Scenario: A paused job
- **WHEN** the agent's max job was paused from the page before the nightly reboot, and the agent polls the next morning
- **THEN** `status` reports it paused, with its checkpoint's time and counters, and exits 0

#### Scenario: A long job killed for memory
- **WHEN** an agent's job is killed for memory seven hours into its search, at a checkpoint's level, and the agent polls
- **THEN** `status` reports it paused, that it died out of memory, the most memory one job gets as what to ask for, and its checkpoint's size among the kept checkpoints, and exits 0; `tlcq resume <id> --memory <size>` recovers it with that memory, and `tlcq cancel <id>` deletes the checkpoint

### Requirement: Fetching a job's files
`tlcq fetch <id>` SHALL write, wherever the job ran, the logs and `result.json` of the run the agent follows (its follow of that id with the highest placement number, else the id's latest run), every attempt's under `./tmp/tlcq/<id>/<run>/<attempt>/` (a laptop run is attempt 0, VM attempts count from 1 and may skip a number an undone or failed-start attempt took, so a moved run has its laptop attempt 0 and VM attempts from 1). For a run that never started (no recorded attempt), it SHALL write nothing, report the run's state (queued, with its position) or outcome, and exit 0.

#### Scenario: A VM job's logs
- **WHEN** an agent fetches a job that ran on the VM
- **THEN** each attempt's TLC log and `result.json` of the run the agent follows are under `./tmp/tlcq/<id>/<run>/<attempt>/`

#### Scenario: A job cancelled while queued
- **WHEN** an agent fetches a VM job cancelled before it started
- **THEN** nothing is written, the report gives `cancelled` with the canceller's label, and `fetch` exits 0

### Requirement: Outcomes and caches
A job's outcome SHALL be `ok`, `unexpected`, `timeout`, `cancelled`, `error`, or `never sent` (a commit whose outcome was unknown never reached the VM). Only `ok` and `unexpected` results SHALL be cached. The VM SHALL cache the results of its own runs; the laptop SHALL cache the results of its runs and every VM result it receives, keeping the first result it holds for an id. When the VM is unreachable, lookups SHALL use the laptop's cache alone.

#### Scenario: Unchanged model
- **WHEN** a model and configuration that already passed are submitted again unchanged
- **THEN** tlcq returns the stored result at once, without running TLC

#### Scenario: Offline
- **WHEN** a model that passed on the VM is submitted again while the VM is unreachable
- **THEN** the laptop's cache returns the result

#### Scenario: Cancelled
- **WHEN** a cancelled job is submitted again unchanged
- **THEN** it runs again


## MODIFIED Requirements

### Requirement: Two slots
The VM SHALL have a 70% slot and a 30% side slot of its cores and memory minus the configured reserve, and run at most one job in each. Admission SHALL be first in, first out: the queue SHALL hold runs in the order of their first commit to the VM (a run undone or whose container failed to start keeping its place), except as a human reorders it in the UI, and "oldest" SHALL mean first in that order. On an idle VM, the oldest waiting job SHALL take both slots (100%), except that a max job SHALL take the 70% slot alone while a non-max job waits that the side slot could run next to it. While either slot is busy, the other SHALL admit on its own: the 70% slot the oldest waiting job; the side slot the oldest waiting non-max job. A job SHALL keep its share until it ends or is restarted, a stopping attempt's share freeing only when the attempt is settled, and a slot held for a restart SHALL take no other job. A job's workers, memory budget (TLC's heap and off-heap memory; tlx's `-maxmem`) and memory limit SHALL follow its share, and SHALL NOT be capped otherwise; a tlx job restarted at another share SHALL recover with that share's workers. A job SHALL start only while free disk is above the configured floor. Admission SHALL run in `commit` (a `now-or-refuse` job starting only if no older job takes the slot it could use), when an attempt is settled or undone, after a `cancel`, on each `status` poll, after a UI action, every minute, and at the runner's start, under one lock that also covers attach and supersede, so a `now-or-refuse` answer and the start it promises are one step. A verb holding that lock SHALL NOT wait for a container to stop, so the stop hook can always take it.

#### Scenario: An idle VM
- **WHEN** a slow job is committed and nothing runs or waits
- **THEN** it starts with all of the VM's cores and memory minus the reserve

#### Scenario: A max job leaves room
- **WHEN** a max job runs at 70% and a short job is committed `now-or-refuse`
- **THEN** the short job starts in the 30% side slot

#### Scenario: Both slots busy
- **WHEN** a max job and a side job run and a short job is committed `now-or-refuse`
- **THEN** the VM refuses it without queueing it, and the client runs it on the laptop

#### Scenario: First in, first out
- **WHEN** the VM is idle and slow job A, slow job B and max job C wait, oldest first
- **THEN** A starts at 100%; when it ends B starts at 100%; when B ends C starts at 100%, since no non-max job waits

#### Scenario: A max job with a non-max job behind it
- **WHEN** the VM is idle and max job C, then slow job D, wait, oldest first
- **THEN** C starts at 70% and D in the side slot

#### Scenario: A max job grows
- **WHEN** max job C runs at 70% and the side slot's job ends with no non-max job waiting
- **THEN** just after TLC's next checkpoint C restarts at 100%, recovering that checkpoint, continuing the same run

#### Scenario: A max job shrinks
- **WHEN** max job C runs at 100% and a slow job is committed
- **THEN** just after TLC's next checkpoint C restarts at 70%, recovering that checkpoint, and the slow job takes the side slot

#### Scenario: A tlx job shrinks
- **WHEN** a max tlx job runs at 100% and a slow job is committed
- **THEN** just after tlx's next checkpoint it restarts at 70% with the 70% slot's workers, recovering that checkpoint, and its generated count goes on from the checkpoint's without being counted twice

#### Scenario: A max job waits
- **WHEN** a max job is committed while a slow job runs at 100%
- **THEN** it waits, and starts at 70% when the slow job ends

#### Scenario: The max job ends first
- **WHEN** a side job runs alone after its max job ended, and a slow job waits
- **THEN** the slow job starts in the 70% slot, with the 2-hour cap

#### Scenario: A non-max job in the 70% slot
- **WHEN** a slow job runs in the 70% slot and the side job ends, and a short job waits
- **THEN** the short job starts in the side slot, and the slow job keeps its 70%

#### Scenario: Low disk
- **WHEN** free disk is below the floor and a slot is free
- **THEN** the job waits, and starts at the first poll after disk is freed

#### Scenario: A job ends
- **WHEN** a running job ends and a job waits for its slot
- **THEN** the waiting job starts without any new submit

### Requirement: One systemd unit per attempt
Each attempt of a VM job (a lane restart starts a new attempt of the same run) SHALL run in its own container and directory, with a memory limit covering its share's memory budget and the engine's overhead beyond it (the JVM's for TLC; for tlx, what its `-maxmem` does not count), so that stopping the container ends the engine and every child, and no attempt waits for an earlier container to go; each container SHALL be named after its id, run and attempt, with `--cpus` and `--memory` set from its share, and the runner SHALL remove it once settled. The supervisor SHALL enforce the time cap: a lane cap counted from the attempt's start, `--timeout` over the run's VM attempts' running times. The VM SHALL name its runs per id, return the run name in its replies, and keep each run's state apart, so a stop hook writes only its own run's outcome. A restart's next attempt SHALL be queued only by the previous attempt's stop hook (or a later settling, if the hook was killed or never ran) applying its restart marker (a restart attempt undone or whose container fails to start SHALL return to its queued restart), and the marker SHALL hold the 70% slot from when it is written until the attempt's end is recorded, the queued restart from then until admission records its attempt or it is dropped; once queued, any admission SHALL be able to start it. A run's duration SHALL add up its attempts' running times, never time queued. When the container exits, its attempt SHALL be settled by the runner under the admission lock: its outcome SHALL be, first match wins, `cancelled` after a cancel, the outcome of the engine's own exit, none after a lane restart (the end recorded as "restarted" and the next attempt queued), `timeout` after its cap, `error` (too big) after a too-big stop, else `error`, except that an attempt whose supervisor never wrote its started marker SHALL be undone (its record deleted, its job queued again) unless a cancel marker ends it `cancelled`, and a run undone the configured number of times in a row (only containers gone before their started marker counting, a started marker resetting the count) SHALL end `error` (never started), written into its last recorded attempt's `result.json`, or kept with its queue entry when it has none, its queued restart and held slot dropped; a failed container start SHALL leave its job queued in its place (a `now-or-refuse` job refused and removed instead) and, either way, end that admission round; attempt numbers SHALL never be reused; the attempt's end SHALL be recorded after its cache and any next attempt, and only then its scratch deleted and admission run; recording the end SHALL free every slot it held (a held 70% slot too, unless the restart was queued). Every verb SHALL take the admission lock, `preflight` included (writing only token tombstones); a lookup of a token the VM has not applied SHALL tombstone it, and a commit carrying a tombstoned token SHALL apply nothing; every verb that writes, and the runner's start, SHALL first settle each recorded attempt whose container is gone and that has no end recorded, and delete the scratch of ended attempts, before applying anything, and read-only verbs SHALL report such an attempt as settling would; settling an attempt twice SHALL change nothing. No job SHALL start while the runner is stopping. At the runner's start, the scratch of ended attempts SHALL be deleted, never a live container's (job containers outlive a runner restart). The queue SHALL survive a reboot and a runner restart. `commit` SHALL be idempotent by id, and a job ended `timeout`, `cancelled` or `error`, or cancelled with its container still stopping, SHALL run anew, as a new run, when its id is committed again. A cancel SHALL end the job for every submitter attached to it.

#### Scenario: Cancel
- **WHEN** an agent cancels a running job that two agents poll
- **THEN** its unit stops, no engine process remains, and both agents' reports say `cancelled`, with the label of the agent that cancelled it

#### Scenario: Out of memory
- **WHEN** a job's unit is killed for exceeding its memory limit
- **THEN** its outcome is `error`, and it is not cached

#### Scenario: A runner restart
- **WHEN** the runner container restarts while a job's container runs
- **THEN** the job keeps running, and the restarted runner settles it when its container exits

#### Scenario: Reboot
- **WHEN** the VM reboots while one job runs and another is queued
- **THEN** the running job ends `error`, not `cancelled`, its scratch is deleted, and the queued one is not started until the VM is back up

#### Scenario: A failed start
- **WHEN** admission starts a queued `now-or-queue` job and its container fails to start
- **THEN** the job stays queued with no attempt recorded and no outcome, and a later admission starts it

#### Scenario: A unit gone before its started marker
- **WHEN** an attempt's unit is gone before its supervisor wrote its started marker, with no cancel marker
- **THEN** settling deletes the attempt's record and queues the job again in its place, under a new attempt number, with no outcome

#### Scenario: A supervisor that never starts
- **WHEN** a job's attempts are undone 3 times in a row, each container gone before its started marker
- **THEN** the run ends `error`, "never started", and the job is not queued again

#### Scenario: A cancel before the started marker
- **WHEN** a job is cancelled after admission recorded its attempt and before its supervisor wrote its started marker
- **THEN** the run ends `cancelled` and never starts again

#### Scenario: A stop hook killed
- **WHEN** a unit's stop hook is killed before it records the attempt's end
- **THEN** the next writing verb or boot settles the attempt before applying anything, and frees its slot

#### Scenario: Rerun after a failure
- **WHEN** a job that ended `error` is committed again
- **THEN** it runs anew under the same id

#### Scenario: The same tarball twice
- **WHEN** `commit` receives an id it already holds queued or running
- **THEN** it stores nothing new and attaches the submitter

### Requirement: The verdict comes from TLC's exit status and the configuration
The engine's exit status SHALL sort a run, by TLC's codes, which tlx shares: 0 and 10–14 go to the verdict; 75–77 and 150–151 are `unexpected`; any other status, a signal or no exit are `error`. A configuration whose leading `\*` comment lines contain `must fail <Property>` or `must fail deadlock` (case-insensitive, the first match) SHALL pass only when the engine exits 10–14 and an `Error:` line names that property as violated or never witnessed (`Deadlock reached` for deadlock). Any other configuration SHALL pass only when the engine exits 0.

#### Scenario: Bug variant
- **WHEN** a configuration says `must fail SafetyB` and TLC reports `SafetyB` violated
- **THEN** the outcome is `ok`

#### Scenario: Another violation first
- **WHEN** a configuration says `must fail SafetyB` and TLC reports another property violated
- **THEN** the outcome is `unexpected`, and the result names the property reported and suggests `--must-fail-only`

#### Scenario: Only the named property
- **WHEN** that configuration is submitted with `--must-fail-only` and `SafetyB` is violated
- **THEN** the outcome is `ok`

#### Scenario: A predicate never witnessed
- **WHEN** a configuration with no `must fail` line lists a `_POSSIBLE` predicate that TLC never witnesses
- **THEN** the outcome is `unexpected`

#### Scenario: TLC runs out of memory
- **WHEN** TLC exits 255 after an out-of-memory error
- **THEN** the outcome is `error`, and it is not cached

#### Scenario: A parse error
- **WHEN** TLC exits 150 on a syntax error in a module
- **THEN** the outcome is `unexpected`, and it is cached

#### Scenario: tlx verdicts as TLC's
- **WHEN** a configuration says `must fail SafetyB` and tlx exits 12 with `Error: Invariant SafetyB is violated.`
- **THEN** the outcome is `ok`

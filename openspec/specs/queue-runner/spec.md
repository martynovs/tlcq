# queue-runner Specification

## Purpose
TBD - created by archiving change tlc-queue. Update Purpose after archive.
## Requirements
### Requirement: Two slots
The VM SHALL have two slots, each pinned to a list of CPUs: the E slot (default CPUs 16–31) and the P slot (default CPUs 0–15), the lists being settings; a human MAY close the P slot (the runner-ui spec). Each open slot SHALL run at most one job, with `-workers` equal to its list's CPU count, and an equal share of the memory less the configured reserve (half with both slots open, all with only the E slot), bounded by what the VM has free when the attempt starts; TLC's heap and off-heap memory and tlx's `-maxmem` SHALL follow the attempt's memory. Admission SHALL be first in, first out: the queue SHALL hold runs in the order of their first commit to the VM (a run undone or whose container failed to start keeping its place), except as a human reorders it in the UI or as a resume or an unfreeze puts a run first, and "oldest" SHALL mean first in that order. The oldest waiting run SHALL take a free open slot, the E slot before the P slot. When an open slot is free and no run can take it (none waits, or the oldest waits for memory), admission SHALL thaw the oldest frozen job a human did not freeze into it, on spare capacity; a job thawed so SHALL hold that slot. A job a human froze SHALL stay frozen, never thawed on spare capacity, until an unfreeze or its pause. When a run waits, no open slot is free and a thawed job holds an open one, admission SHALL freeze that job again and start the run in its slot, but only if the run's memory, with the frozen job's unused limit no longer promised, reaches the minimum; closing the P slot SHALL freeze a thawed job in it at once. A pausing job SHALL NOT be frozen: a pausing job in a closed P slot keeps it until its pause lands. A frozen or paused job SHALL hold no slot. A frozen job SHALL keep the memory it uses, the rest of its memory limit SHALL NOT be promised to other attempts while it is frozen, and it SHALL thaw, on spare capacity or by an unfreeze, whatever the memory: what it uses is already taken. A job SHALL keep its slot until it ends, is frozen or is paused, a stopping attempt's slot freeing only when the attempt is settled. A job SHALL start only while free disk is above the configured floor. While free disk is below twice the floor, the runner SHALL, every minute, delete the scratch directories no live attempt owns and the checkpoints no run that has not ended owns, and prune the images no container uses and no tag names; it SHALL delete nothing a live or paused run needs. Admission SHALL run in `commit` (a `now-or-refuse` job starting only if an open slot is free for it now, a slot held by a thawed job counting as free), when an attempt is settled or undone, after a `cancel`, on each `status` poll, after a UI action, every minute, and at the runner's start, under one lock that also covers attach and supersede, so a `now-or-refuse` answer and the start it promises are one step. A verb holding that lock SHALL NOT wait for a container to stop, so the stop hook can always take it.

#### Scenario: An idle VM
- **WHEN** a slow job is committed and nothing runs or waits
- **THEN** it starts in the E slot, with the E slot's CPUs and workers and half the memory

#### Scenario: A second job
- **WHEN** a job runs in the E slot and a short job is committed `now-or-refuse`
- **THEN** the short job starts in the P slot

#### Scenario: Both slots busy
- **WHEN** jobs run in both slots and a short job is committed `now-or-refuse`
- **THEN** the VM refuses it without queueing it, and the client runs it on the laptop

#### Scenario: First in, first out
- **WHEN** the VM is idle and slow job A, slow job B and max job C wait, oldest first
- **THEN** A starts in the E slot and B in the P slot; C starts in whichever ends first

#### Scenario: Two max jobs
- **WHEN** max jobs A and B run in both slots and a short job waits
- **THEN** the short job waits until one ends, or until a human freezes or pauses one

#### Scenario: Only the E slot
- **WHEN** the P slot is closed, a job runs in the E slot and another waits
- **THEN** the waiting job starts when the E slot frees, never in the P slot

#### Scenario: Spare capacity and new work
- **WHEN** a frozen job runs on spare capacity in the P slot, the E slot is busy, and a job is committed
- **THEN** the frozen job is frozen again and the new job starts in the P slot

#### Scenario: Closing P under a thawed job
- **WHEN** a frozen job runs on spare capacity in the P slot and a human closes the P slot
- **THEN** the job is frozen again at once, and nothing runs in the P slot

#### Scenario: Low disk
- **WHEN** free disk is below the floor and a slot is free
- **THEN** the job waits, and starts at the first poll after disk is freed

#### Scenario: A job ends
- **WHEN** a running job ends and a job waits for its slot
- **THEN** the waiting job starts without any new submit

### Requirement: Lane decisions and time caps
A VM job's lane SHALL move up at most one step per decision, and its supervisor SHALL make each decision when it falls due, whether or not anyone polls, and record it in the job's result; `status` SHALL report it at once, and the laptop SHALL record it in the history when the run ends. The lane timers (the 15-minute decisions) SHALL count from the run's first VM attempt's start, never from a laptop attempt, time queued, frozen or paused. A short job still running at 15 minutes SHALL become slow, whatever its probe says. A slow or unknown job SHALL be probed 15 minutes after it entered the slow lane: `big` (queue / distinct ≥ 0.4, or queue growth over the last 5 minutes ≥ 0.1 of the distinct states) makes it max in place, in the slot it holds, with max's cap; `ok` keeps it slow. A max job's probe SHALL be reported, never acted on. Probe times and thresholds SHALL be configurable per lane, and every probe's numbers SHALL be recorded. A VM job SHALL end `timeout` when it passes its lane's cap: 2 hours for slow or unknown ones, 12 hours for max ones, in either slot, counted over the run's VM attempts' running times, never time frozen or paused; an attempt after a resume SHALL keep the run's lane and count its timers and cap on from there. The caps SHALL be configurable, and `--timeout` SHALL replace every lane cap of its job, survive lane changes and count the same way; on the laptop it SHALL apply only with `--local`.

#### Scenario: A short job runs long
- **WHEN** a short job is still running at 15 minutes
- **THEN** it becomes slow with the 2-hour cap, the next `status` says so, and history records long when the run ends

#### Scenario: Short never jumps to max
- **WHEN** a short job still running at 15 minutes has a `big` probe
- **THEN** it becomes slow, not max, and is probed again 15 minutes later, at 30 minutes

#### Scenario: Big
- **WHEN** a slow job in the P slot is judged `big` while a max job runs in the E slot
- **THEN** it becomes max in place, keeps running in the P slot, and gets max's 12-hour cap

#### Scenario: Nobody polls
- **WHEN** an unknown job reaches its probe time and no agent has polled it
- **THEN** its supervisor decides and records the lane all the same, and the next poll reports it

#### Scenario: Frozen time
- **WHEN** a slow job ran 1 hour, was frozen for 3 hours, and was then unfrozen
- **THEN** it has 1 hour left before its 2-hour cap

#### Scenario: A resumed max job
- **WHEN** a max job ran 11 hours, was paused overnight and is resumed
- **THEN** it is still max, and ends `timeout` after 1 more hour of running

#### Scenario: An override
- **WHEN** a max job is submitted with `--timeout 24h`
- **THEN** it is stopped only after 24 hours of running time

### Requirement: One systemd unit per attempt
Each attempt of a VM job (a resume after a pause starts a new attempt of the same run) SHALL run in its own container and directory, with a memory limit covering its slot's memory budget and the engine's overhead beyond it (the JVM's for TLC; for tlx, what its `-maxmem` does not count), so that stopping the container ends the engine and every child, and no attempt waits for an earlier container to go; each container SHALL be named after its id, run and attempt, with `--cpuset-cpus` set to its slot's CPU list and `--memory` from its slot's memory, and the runner SHALL remove it once settled. A frozen attempt's container SHALL be paused, not gone: it SHALL stay its run's live attempt, hold no slot, and be unpaused only by admission's thaw, with its CPU list set to the slot it thaws into, or by its own pause, which ends its freeze; its freezes, thaws and pause's unpause SHALL be appended, with their times, to a log in its directory. A pausing job SHALL NOT be thawed or unfrozen. A frozen, thawed, pausing or paused run, and a resumed or unfrozen run waiting first in the queue, SHALL be live for preflight, attach, older versions and commit by id, and SHALL read as running to a laptop that predates these states. The supervisor SHALL enforce the time cap: the lane cap or `--timeout` over the run's VM attempts' running times, never time frozen or paused. The VM SHALL name its runs per id, return the run name in its replies, and keep each run's state apart, so a stop hook writes only its own run's outcome. A pause SHALL stop an attempt only through its supervisor, which writes a paused stop marker under the admission lock just before stopping the engine; a pause request with no stop marker SHALL change no outcome. A paused run SHALL hold no slot and keep its checkpoint, a tlx one with a copy of the tlx binary that wrote it, which its recovery SHALL run (tlx recovers a checkpoint only with the binary that wrote it, and the image's may be newer). A long run (its duration at least an hour) whose engine dies on its own with no verdict (killed for memory, say) or with an `error`, or that its supervisor stops at its time cap, SHALL keep its checkpoint as a pause does, and its attempt SHALL settle with none, ranking just after a cancel: the run paused on that checkpoint with the reason shown (with the memory to ask for, up to the most one job gets, when killed for memory), until its agent resumes it, with more memory or a longer cap if it asks (`tlcq resume --memory`, `--timeout`, a cap at or below the time it has run refused), or drops it (`tlcq cancel`, or a supersede by a new version), so hours of search are never lost to one death; its supervisor SHALL keep it, or, when the supervisor died with its engine (a shutdown, a reboot) and wrote no end, settling SHALL keep it from the scratch, with no binary (its recovery then runs the image's tlx); its next attempt SHALL be queued only by a resume (a resumed attempt undone or whose container fails to start SHALL return to the head of the queue); once queued, any admission SHALL be able to start it. A run's duration SHALL add up its attempts' running times, never time queued, frozen or paused. When the container exits, its attempt SHALL be settled by the runner under the admission lock: its outcome SHALL be, first match wins, `cancelled` after a cancel, none when a resume's engine exits on its own before its log says the recovery of the run's kept checkpoint completed (the run paused again on that checkpoint, the engine's reason shown), the outcome of the engine's own exit, none after its paused stop marker when the run's checkpoint was kept (the end recorded as "paused", the run paused; with no checkpoint kept, `error`), `cancelled` after its no-checkpoint stop (a frozen job paused before its first checkpoint, "paused before its first checkpoint"), `timeout` after its cap, none when an attempt that recovered the run's kept checkpoint (still on disk) died with nothing to say (the run paused again on that checkpoint, for another resume, the signal that killed its engine shown as the reason), else `error` (naming that signal when there is one), except that an attempt whose supervisor never wrote its started marker SHALL be undone (its record deleted, its job queued again) unless a cancel marker ends it `cancelled`, and a run undone the configured number of times in a row (only containers gone before their started marker counting, a started marker resetting the count) SHALL end `error` (never started), written into its last recorded attempt's `result.json`, or kept with its queue entry when it has none; a failed container start SHALL leave its job queued in its place (a `now-or-refuse` job refused and removed instead) and, either way, end that admission round; attempt numbers SHALL never be reused; the attempt's end SHALL be recorded after its cache and any next attempt, and only then its scratch deleted and admission run; recording the end SHALL free the slot it held. Every verb SHALL take the admission lock, `preflight` included (writing only token tombstones); a lookup of a token the VM has not applied SHALL tombstone it, and a commit carrying a tombstoned token SHALL apply nothing; every verb that writes, and the runner's start, SHALL first settle each recorded attempt whose container is gone and that has no end recorded, and delete the scratch of ended attempts, before applying anything, and read-only verbs SHALL report such an attempt as settling would; settling an attempt twice SHALL change nothing. No job SHALL start while the runner is stopping. At the runner's start, the scratch of ended attempts SHALL be deleted, never a live container's (job containers outlive a runner restart). The queue SHALL survive a reboot and a runner restart. `commit` SHALL be idempotent by id, and a job ended `timeout`, `cancelled` or `error`, or cancelled with its container still stopping, SHALL run anew, as a new run, when its id is committed again. A cancel SHALL end the job for every submitter attached to it.

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

#### Scenario: A paused attempt
- **WHEN** a job's supervisor stops its engine for a pause just after a checkpoint
- **THEN** settling records the attempt's end as "paused" with no outcome, frees its slot, keeps the run's checkpoint, and caches nothing

#### Scenario: A frozen job at a reboot
- **WHEN** the VM reboots while one job is frozen and another is paused
- **THEN** the frozen job ends `error`, as a running one does, and the paused one is still paused after the reboot

#### Scenario: Cancelling a frozen job
- **WHEN** an agent cancels a job that is frozen
- **THEN** its paused container is killed, and the job ends `cancelled` once settled

#### Scenario: Gone while pausing
- **WHEN** a pause was requested for a running job, and the VM reboots before its next checkpoint
- **THEN** the job ends `error`, not paused

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


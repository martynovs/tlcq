## MODIFIED Requirements

### Requirement: Records and preflight
Placement, supersede, cancel and moves on the laptop SHALL be serialised: commands place one at a time, from the preflight to their last record write, and SHALL NOT hold that turn afterwards; `status` and `cancel` SHALL apply their laptop changes in the same order, and only if the job's record has not changed since they read the VM. A command SHALL check its input, the VM's tarball checks included, and compute its ids before taking its turn, and write none of its own job records or entries before every refusal is decided (the refresh below is exempt). It SHALL make one read-only preflight call to the VM; a preflight that connects and then fails SHALL exit 3 with nothing of its own written. The preflight SHALL also refresh every laptop record of a job sent to the VM: a job the VM holds stays live (a VM attempt whose container is gone counts as the VM would settle it); a cached or ended one gets its result or outcome; a job whose send's outcome was unknown and that the VM does not know ends `never sent`, and an agent whose send reached no run (by its token, once a lookup has made its token final (the VM then never applies that commit)) is reported `never sent` even if the id runs for another worktree; a job the laptop thought was on the VM and the VM does not know ends `error`; a failed move ends `timeout`; while the VM is unreachable, every such record stays live. Each job's expectation SHALL be computed after this refresh. Each remaining job SHALL then be a cache hit, an attach (to the VM's queued or running job, or to a live laptop job), or new. A laptop job's lock SHALL be held from before its record is written, and pass to its supervisor without being free in between. Each job SHALL keep one entry per submitter (worktree path, model directory within it, spec module, configuration name, label); the entries of a job on the VM SHALL live on the VM, with a mirror on the laptop for when the VM is unreachable, and an entry added offline SHALL reach the VM before any later supersede, and only on the run it attached to (for a job sent with an unknown outcome, the run that send reached), while that run is live.

#### Scenario: Resubmit after a laptop timeout
- **WHEN** a job that ended `timeout` on the laptop is submitted again unchanged
- **THEN** it is placed anew, expected long, on the VM

#### Scenario: Resubmit after the VM job ended
- **WHEN** a job left in state `vm` is submitted again after the VM finished it `ok`
- **THEN** the VM's result is copied to the laptop's cache and returned

#### Scenario: Attach on the laptop to a VM job
- **WHEN** a second worktree attaches to a job the laptop knows is on the VM, and the first worktree later supersedes it
- **THEN** the VM still holds the second worktree's entry, and the job keeps running for it

#### Scenario: Attach while offline
- **WHEN** a worktree attaches to a VM job while the VM is unreachable, and later the first worktree supersedes it
- **THEN** the attach's entry reaches the VM first, and the job keeps running for the attached worktree

#### Scenario: A crashed command
- **WHEN** a `submit` is killed before it writes any record, and the same job is submitted again
- **THEN** nothing was left behind, and the job is placed anew

#### Scenario: Killed just before the commit
- **WHEN** a `submit` is killed after its write-ahead `sent?` record and before its commit, and its agent polls
- **THEN** once a lookup has made its token final (the VM then never applies that commit), the agent's next `status` finds its follow `never sent` and exits 3 (for a new job the record ends `never sent` too; for an attach to a VM job the record becomes `vm` again), and a resubmit places the job anew, or attaches to the running job

#### Scenario: Killed after the commit
- **WHEN** a `submit` is killed after its commit but before writing its records, and the job is submitted again
- **THEN** the preflight finds it on the VM, the new command attaches to it and writes its record, and it does not run on the laptop

#### Scenario: An unknown commit, then the VM goes away
- **WHEN** a commit's outcome was unknown, and the job is submitted again while the VM is unreachable
- **THEN** the new command attaches to the record and keeps polling, and does not run the job on the laptop

#### Scenario: A commit that never arrived
- **WHEN** a commit's outcome was unknown, and the VM, reachable again, does not know the job
- **THEN** a `status` on it reports outcome `never sent`, and a resubmit places it anew

#### Scenario: Skipped jobs take no part
- **WHEN** `tlcq submit november` skips a history-max configuration whose older version is running
- **THEN** that older version neither refuses the command nor is superseded

#### Scenario: A job ends before the commit
- **WHEN** the preflight saw a job running on the VM, and it ended `timeout` before the commit
- **THEN** the commit queues it anew rather than attaching to the ended run

#### Scenario: A refused batch attaches nothing
- **WHEN** a batch holds a job attaching to a VM job, or to one while the VM is unreachable, and another job that is refused
- **THEN** no entry is added on the VM, no pending entry is stored, and none of the command's own records is written

#### Scenario: A preflight cut off
- **WHEN** the preflight connects and the connection then drops
- **THEN** the command exits 3, writes none of its own records, and runs nothing

#### Scenario: A preflight after a killed stop hook
- **WHEN** a VM attempt's unit is gone, its stop hook was killed before recording its end, and a preflight reads its job
- **THEN** the preflight reports the job as settling would (its outcome, or paused when a pause stopped it), not as running

### Requirement: Older versions
An older version of a new job SHALL be a queued or running job, on either machine, with an entry matching the new job's worktree path, model directory, spec module and configuration name, but another id; jobs from other worktrees SHALL NOT count. On the laptop, a job waiting for the slot or for disk SHALL count as queued, and one running or moving as running; a VM run that is frozen, thawed, pausing or paused, or resumed or unfrozen and waiting first in the queue, SHALL count as running; while the VM is unreachable, one the VM may hold (sent to it, or sent with an unknown outcome) SHALL count as running. Jobs skipped as max SHALL take no part in this rule. If an older version is running and the new job is no cache hit (an attach counts, since it adds this worktree to a running job), the command SHALL be refused with a report of that job (id, place, elapsed time, latest counters), unless `--replace` is given; `--replace` SHALL be refused while it cannot reach the job. After every refusal is decided, each queued older version and each `--replace` target of a new job that is no cache hit SHALL be superseded (a cache hit supersedes nothing): every entry matching the new job's key removed, and a job left with no entry cancelled, ending `cancelled` with "replaced by" the new id; the command SHALL report each. A supersede SHALL also drop the worktree's pending entries on the superseded job, before the commit. Laptop supersedes SHALL happen before the commit; the VM's supersedes SHALL go in the commit call. A job about to be sent to the VM SHALL be recorded on the laptop, as sent with an unknown outcome and with the command's own entry, before the commit call, and recorded by its answer after it: on the VM, ended with the result when cached, or on the laptop when refused.

#### Scenario: Running older version
- **WHEN** an agent edits a spec and resubmits a configuration whose older version runs on the VM
- **THEN** tlcq exits 2 and reports the running job, and the new one is not placed

#### Scenario: Replace a running one
- **WHEN** the agent submits it again with `--replace`, and no other worktree is attached to the older job
- **THEN** the older job ends `cancelled`, "replaced by" the new id, and the new job is placed

#### Scenario: Replace a shared one
- **WHEN** the agent submits with `--replace` and another worktree is attached to the running older job
- **THEN** it keeps running for that worktree, the command reports so, and the new job is placed

#### Scenario: Queued older version
- **WHEN** the older version is only queued, on the VM or for the laptop's slot, and has never run
- **THEN** it ends `cancelled`, "replaced by" the new id, is reported, and adds no history record

#### Scenario: Two commands at once
- **WHEN** two commands from one worktree submit two versions of a configuration at the same moment
- **THEN** the second places after the first and finds the first's job as an older version: superseded if queued, refused if running

#### Scenario: A refused command
- **WHEN** a command with a queued older version is refused for another job
- **THEN** the queued older version stays queued

#### Scenario: Reverted to a cached text
- **WHEN** an older version runs or is queued and the new job's id has a cached result
- **THEN** the cached result is returned, and the older version keeps running or stays queued

#### Scenario: VM unreachable
- **WHEN** this worktree's older version was sent to the VM and the VM is now unreachable
- **THEN** a new run of that configuration is refused, with `--replace` too, and the report says the VM is unreachable

#### Scenario: Supersede outcome unknown
- **WHEN** the connection drops during a commit that supersedes an older version on the VM
- **THEN** the command starts nothing on the laptop and exits 3; a rerun sees the VM's real state

#### Scenario: A laptop older version and a lost reply
- **WHEN** a new version is committed to the VM, its older version runs on the laptop, and the commit's reply is lost
- **THEN** the older laptop run was already superseded before the commit, so the two never run together

#### Scenario: Attaching beside one's own running version
- **WHEN** worktree w1 has version b running, and submits version a, which worktree w2 is running
- **THEN** the attach is refused like a new run, unless `--replace`, so w1 never has two versions running

#### Scenario: An offline attach to a job already superseded
- **WHEN** a worktree attaches offline to a job that another worktree had already superseded on the VM
- **THEN** once the VM is reachable, its `status` reports the job `cancelled`, "replaced by" the other worktree's id, and exits 3, so the agent resubmits

#### Scenario: A lost reply, then a new version offline
- **WHEN** a commit's reply is lost, the VM becomes unreachable, and the same worktree submits a new version of that configuration
- **THEN** the write-ahead record's entry shows the job as an older version, counted as running, and the new version is refused

#### Scenario: Killed after the commit, then offline
- **WHEN** a `submit` is killed right after its commit, the VM then becomes unreachable, and the job is submitted again
- **THEN** the laptop's write-ahead record makes the new command attach and poll, and the job does not run on the laptop

#### Scenario: Other worktrees and directories
- **WHEN** another worktree, or a copied model directory in this worktree, has a job with the same spec module and configuration name
- **THEN** it is not an older version, and both run

#### Scenario: A paused older version
- **WHEN** an agent submits a new version of a configuration whose older version is paused on the VM
- **THEN** the command is refused as for a running older version; with `--replace` the paused run is cancelled and its checkpoint deleted

#### Scenario: A resumed older version waiting
- **WHEN** an agent submits a new version of a configuration whose older version was resumed and waits first in the queue for a slot
- **THEN** the command is refused as for a running older version, and the resumed run keeps its checkpoint


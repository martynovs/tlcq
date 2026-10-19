# job-submit Specification

## Purpose
TBD - created by archiving change tlc-queue. Update Purpose after archive.
## Requirements
### Requirement: One command
`tlcq submit <dir> [cfg…]` SHALL submit one job per configuration named (configurations with the same id SHALL be one job, with an entry per configuration name, expected as the longest of its names (short < unknown < long < max) and skipped as max only if every name is), decide where each job runs, and return at once with the report the job-status spec describes. With none named, it SHALL submit every `*.cfg` in the directory except those expected max by their suffix or history, and list those with their cached outcome, if any; `--all` SHALL include them. No command SHALL block until jobs end; the agent polls `tlcq status`. The agent SHALL NOT have to name a machine. A command SHALL decide every refusal before it sends, runs, supersedes or cancels anything; if any job is refused, it SHALL exit 2, name each refused job and why, and do none of these.

#### Scenario: A mixed batch
- **WHEN** an agent runs `tlcq submit november` with short, long and unknown configurations among them
- **THEN** each job is placed by its expected duration, and the command returns at once with each job's id, machine and state

#### Scenario: One job refused
- **WHEN** a batch holds short jobs and one long job while the VM is unreachable
- **THEN** tlcq exits 2, names the long job and suggests `--local`, and runs none of the jobs

#### Scenario: Max configurations skipped
- **WHEN** an agent runs `tlcq submit november` with no configuration named
- **THEN** the max configurations do not run, and the output lists them with their cached outcomes

#### Scenario: Two names, one text
- **WHEN** `a_max.cfg` and `b.cfg` have the same text and `tlcq submit november` names neither
- **THEN** they are one job expected max, carrying both names; had every name been max it would be skipped, but `b` has no max history, so it runs, as max

#### Scenario: Everything
- **WHEN** an agent runs `tlcq submit november --all`
- **THEN** the max configurations run too

### Requirement: A submission config in TOML
A submission SHALL be describable in TOML with the keys `spec`, `extends` (directories relative to the model directory whose modules the spec reuses), `cfgs` (names, or tables with `name` and per-configuration `expect`, `timeout`, `must_fail_only`, `tlc_opts`, `engine`), and the flags of `submit` as keys, which per-configuration fields override. It SHALL be read, each layer overriding the one before key by key, from `<dir>/tlcq.toml` if present, then from `--config <file>` or from stdin with `--config -`, then from the command line, whose configuration names replace `cfgs`, each named configuration keeping the per-configuration fields of the entry of that name in the layers below, if there is one. A flag given on the command line SHALL override the per-configuration fields of every configuration too, named or not: in a file, a per-configuration field beats a top-level key; on the command line, the flag wins. tlcq SHALL refuse unknown keys, an `engine` other than `tlx` or `tlc`, and a submission with no `spec` from any layer, exiting 2.

#### Scenario: Config on stdin
- **WHEN** an agent runs `tlcq submit juliet --config -` with `spec = "Juliet"`, `extends = ["../november"]` and three `cfgs`, one with `must_fail_only = true`, piped in
- **THEN** the three jobs are submitted, November's modules travel with them, and only that one is rewritten to its named property

#### Scenario: The directory's defaults
- **WHEN** `juliet/tlcq.toml` gives `spec` and `extends`, and an agent runs `tlcq submit juliet JA.cfg --replace`
- **THEN** JA.cfg is submitted with the directory's spec and extends, replacing its older version

#### Scenario: No spec anywhere
- **WHEN** an agent submits a directory with no `tlcq.toml` and no `--config`
- **THEN** tlcq exits 2 and says the spec module is not named

#### Scenario: A named configuration keeps its entry
- **WHEN** `tlcq.toml` gives `{ name = "NB_08_live.cfg", engine = "tlc", timeout = "24h" }` and an agent runs `tlcq submit november NB_08_live.cfg`
- **THEN** NB_08_live.cfg runs on TLC with the 24-hour cap

#### Scenario: The flag beats a configuration's engine
- **WHEN** `tlcq.toml` gives `{ name = "NB_08_live.cfg", engine = "tlc" }` and an agent runs `tlcq submit november NB_08_live.cfg --engine tlx`
- **THEN** NB_08_live.cfg runs on tlx

### Requirement: A job is the modules and one configuration
A job SHALL hold every `*.tla` in the model directory and in the directories the config's `extends` names, except `*_TTrace_*.tla`, plus the one configuration it checks, and no other file. It SHALL be sent as a tarball on the runner command's stdin, laid out as `model/` (with the configuration as `model/job.cfg`) and `lib/<n>/` per dependency directory. The command line SHALL carry only fixed verbs and ids matching `^[a-z0-9-]+$`. On either machine, the job's engine SHALL run on the extracted tarball in the job's own directory, with absolute paths, never in the worktree. The VM SHALL refuse, before storing anything, a tarball with an absolute path, a `..` component, a name outside `^[A-Za-z0-9_.-]+$`, an entry other than a regular file or directory, or more bytes or entries than the limits.

#### Scenario: Uncommitted model
- **WHEN** an agent submits a model directory with uncommitted and untracked files
- **THEN** the job checks exactly the modules and configuration in the directory at submit time

#### Scenario: An edit during a run
- **WHEN** a module is edited in the worktree while its job runs
- **THEN** the run and its result are unchanged, and no file of the engine's appears in the worktree

#### Scenario: A dependency outside the model directory
- **WHEN** the config names `../november` in `extends`
- **THEN** its modules travel as `lib/0/` and the engine finds them through its module search path (TLC's library path, tlx's `-I`)

#### Scenario: Hostile entries
- **WHEN** a tarball holds an absolute path, a `..` component, a bad name, a link, or more bytes or entries than the limits
- **THEN** the VM refuses the job before storing it

### Requirement: A job's id is the hash of what it checks
A job's id SHALL be the SHA-256 of a manifest of the spec module name, the tarball's sorted paths each with the SHA-256 of its bytes, the options, and the engine's identity: for TLC, `tlc` and the jar version (the SHA-256 of `tla2tools.jar` and `CommunityModules-deps.jar`); for tlx, `tlx` and the git rev it was built from. The configuration's name, resource settings, time caps and submitters SHALL NOT be part of it. Each machine SHALL compute the jar version from its own jars and read the tlx rev recorded beside its own tlx binary, and the VM SHALL recompute the id and refuse a job whose engine identity differs from its own. Only allowlisted options (`--tlc-opt`) SHALL be accepted.

With `--must-fail-only`, the client SHALL rewrite each configuration that has a `must fail` header, before hashing it, to check only the named property: the leading `\*` block kept verbatim, every other `INVARIANT(S)`, `PROPERTY`/`PROPERTIES` and `_POSSIBLE` entry dropped, and exactly one `CHECK_DEADLOCK` line, `TRUE` for deadlock and `FALSE` otherwise; configurations without the header SHALL be left as they are. Under this flag, a `must fail` naming a property other than deadlock that the configuration does not list SHALL be refused.

#### Scenario: Another configuration in the directory changes
- **WHEN** one configuration of a model directory is edited
- **THEN** the ids of the directory's other configurations stay the same

#### Scenario: Two worktrees
- **WHEN** two worktrees submit the same model text and configuration
- **THEN** both get the same id, and the second attaches to the first's job

#### Scenario: Another spec module
- **WHEN** the same files are submitted with another spec module named in the config
- **THEN** the id differs

#### Scenario: An option off the allowlist
- **WHEN** a job asks with `--tlc-opt` for `-metadir`, `-continue`, `-simulate` or any other option off the allowlist
- **THEN** tlcq refuses it, exit code 2

#### Scenario: Stale jars
- **WHEN** the laptop's jars differ from the VM's and a TLC job is submitted
- **THEN** the command is refused at the preflight and says to align the jars

#### Scenario: Stale tlx
- **WHEN** the laptop's tlx rev differs from the VM's and a tlx job is submitted
- **THEN** the command is refused at the preflight and says to install the same tlx rev on both sides

#### Scenario: Another engine
- **WHEN** the same files and configuration are submitted once with `--engine tlx` and once with `--engine tlc`
- **THEN** the two jobs have different ids, and neither is answered from the other's cached result

#### Scenario: Only the named property
- **WHEN** a `must fail SafetyB` configuration with `CHECK_DEADLOCK FALSE` and a multi-line `INVARIANTS` list is submitted with `--must-fail-only`
- **THEN** its `job.cfg` keeps the header, has one `CHECK_DEADLOCK FALSE` line and only `SafetyB`, and its id differs from the plain run's

### Requirement: Expected duration
A job SHALL be expected short, long, max or unknown: by a flag when given (`--fast` short, `--slow` long, `--max` max), else by its configuration name suffix (`_fast`, `_slow`, `_max` likewise), else by the first match in the laptop's history: spec hash, configuration hash and options hash; then spec module name, configuration hash and options hash; then spec module name and configuration name. These lookups SHALL run first over the records of the job's own engine, and only when none matches over TLC's records, the expectation then reported as from TLC; a record made before records carried an engine SHALL count as TLC's. A name match SHALL be used only when it makes a job long or max. A duration (a flag, or an `expect` key) SHALL be refused unless configurations are named or `--all` is given. A finished run SHALL record its duration; a run that ended early SHALL record a lower bound (long at or past 5 minutes, max at or past 1 hour, nothing below 5 minutes). There SHALL be one record per job id and end, however many calls report it, and a lower bound SHALL never lower an expectation. A lane decision of the VM (D9) carried in a result SHALL be recorded as it is, whatever the run's duration: a job judged max is recorded max. The laptop SHALL record its own runs and every VM result it learns of, the preflight included. Durations SHALL be measured in awake time, and SHALL NOT include time queued. tlcq SHALL warn when the history contradicts a suffix, and SHALL NOT rename files.

#### Scenario: A spec edit keeps the hint
- **WHEN** a configuration that took two hours is submitted again after an edit to the spec
- **THEN** it is expected max, from the record under its configuration hash

#### Scenario: A constant changed
- **WHEN** a configuration whose earlier version timed out on the laptop is submitted with an edited constant
- **THEN** its name record makes it long and it is queued on the VM

#### Scenario: An early crash
- **WHEN** a run ends `error` after 10 seconds
- **THEN** no record is added, and the expectation is unchanged

#### Scenario: A slow-lane timeout
- **WHEN** a job expected long reaches the VM's 2-hour cap
- **THEN** its outcome is `timeout`, its record says max, and a resubmit runs as max

#### Scenario: A short cancel after a long run
- **WHEN** a configuration that took 3 hours is cancelled after 6 minutes on its next run
- **THEN** it is still expected max

#### Scenario: A timeout no one waited for
- **WHEN** a job sent with `submit` timed out on the VM and is submitted again
- **THEN** the preflight reports the timeout, its record is added, and the job is placed a lane up

#### Scenario: A first tlx run placed from TLC's history
- **WHEN** a configuration with no tlx record took 1 minute on TLC, and is submitted with the default engine
- **THEN** it is expected short, from TLC, and placed as a short job

#### Scenario: The engine's own history first
- **WHEN** a configuration took 2 hours on TLC and 4 minutes on tlx, and is submitted on tlx
- **THEN** it is expected short, from the tlx record

#### Scenario: A flag overrides
- **WHEN** a configuration whose history says short is submitted by name with `--max`
- **THEN** it is expected max

#### Scenario: A duration flag without names
- **WHEN** an agent runs `tlcq submit november --max`
- **THEN** tlcq exits 2 and says a duration flag needs named configurations or `--all`

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

### Requirement: Placement
Each new job SHALL be placed: a cached result if the laptop or the VM has one; else an attach to the VM's queued or running job with the same id, whether or not the laptop has a record of it; else a long or max job to the VM queue; an unknown job to the laptop if it holds a laptop slot, else to the VM queue; a short job to the VM if the VM starts it now (`now-or-refuse`, D9), else to the laptop if it holds a laptop slot, else to the VM queue. The laptop's free slots SHALL be handed out once per command, before the commit, in submit order, unknown jobs first. An attach to a laptop job SHALL add its entry on the laptop only. After every refusal is decided, everything a command changes on the VM SHALL go in one call with an overall deadline, applied in order: pending entries, jobs (attaches included), supersedes, admission; the VM SHALL validate every job in it before applying anything, and abort the whole call, exiting 2, if any is invalid. If the call's outcome is unknown (a failure to connect included), the command SHALL run nothing on the laptop, report it, and exit 3, and its jobs SHALL be resolved by the next preflight or `status`. When the preflight cannot connect (the VM is unreachable), short and unknown jobs SHALL wait for the laptop's slot, and long or max jobs SHALL be refused with a suggestion of `--local`. `--local` SHALL run on the laptop, uncapped, every job of the command that is no cache hit and no attach. An attached job SHALL keep the first submitter's flags, with a warning when the attaching command's differ.

#### Scenario: VM busy
- **WHEN** a short job is submitted, no VM slot is free and the laptop's slot is free
- **THEN** it runs on the laptop

#### Scenario: One free slot, two candidates
- **WHEN** one unknown and one short job are submitted together, the laptop has one free slot, and the VM is full
- **THEN** the unknown job takes the laptop's slot and the short one is queued on the VM

#### Scenario: An invalid job in a batch
- **WHEN** one tarball of a commit fails the VM's checks
- **THEN** nothing in the commit is applied, no older version on the VM is superseded, the command's new `sent?` records and their follows are removed and any it rewrote restored, its cache hits stay, any laptop older version already superseded is reported, and the command exits 2 naming that job

#### Scenario: Unknown, laptop busy
- **WHEN** an unknown configuration is submitted while the laptop's slot is busy
- **THEN** it goes to the VM queue, with the long lane's cap

#### Scenario: Offline batch
- **WHEN** ten short jobs are submitted while the VM is unreachable
- **THEN** they run on the laptop one at a time, in submit order, and the command's turn to place ends before they run

#### Scenario: VM down, forced
- **WHEN** a long job is submitted with `--local` while the VM is unreachable
- **THEN** it runs on the laptop with no time cap

#### Scenario: A dropped connection
- **WHEN** the connection drops during the commit call
- **THEN** no job is run on the laptop, the command exits 3, and the next preflight finds each job on the VM or ends it `never sent`

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

### Requirement: Runs on the laptop
The laptop SHALL run at most one job at a time (configurable). Every laptop job, waiting or running, SHALL have a detached supervisor that outlives the agent's command and holds its job's lock; waiting jobs SHALL take the slot in submit order, and start only above the free-disk floor. A laptop run, unless forced with `--local` (then uncapped, or capped by `--timeout`), SHALL be stopped after 5 minutes of awake time and recorded as a timeout; the job SHALL end `timeout`, or with `--vm-on-timeout` move to the VM queue under the same id, expected long, with all its entries. A cancel SHALL apply to a job running, waiting for the slot, or queued on the VM. A laptop job, waiting or running, found with its lock free SHALL end `cancelled` if a cancel marker is in its directory, `timeout` if a timed-out marker is, else `error`, and its scratch SHALL be deleted. A cancel of a job whose place is uncertain (its send's outcome unknown, a move unresolved, or any VM job while the VM is unreachable) SHALL be refused, exit 2, as SHALL a cancel of an id neither machine knows. A cancel of a laptop job, or of a job queued on the VM, SHALL complete before it returns (exit 0); a cancel of a job running on the VM SHALL request the stop and return (exit 0), the job ending `cancelled` when its container has exited. A cancel SHALL leave an ended job as it is (exit 0, reporting its outcome), and exit 3 when its VM call connected and then failed. A laptop timeout SHALL leave a timed-out marker before anything else. A job run anew under the same id SHALL start with none of the earlier run's markers or results. A laptop job ended by a kill SHALL still record its lower bound in the history, from the awake time its supervisor last wrote. A move whose commit outcome is unknown SHALL be resolved later: the job becomes a VM job if the VM holds it, else it ends `timeout`.

#### Scenario: Unknown overrun
- **WHEN** an unknown configuration is still running on the laptop after 5 minutes
- **THEN** it is stopped, its timeout is recorded, and the next `status` reports outcome `timeout` and exits 3

#### Scenario: Unknown overrun, moved
- **WHEN** the same happens with `--vm-on-timeout`
- **THEN** the job is queued on the VM as long under the same id, and the agent's reports follow it there

#### Scenario: Laptop restart
- **WHEN** the laptop restarts while a job runs on it
- **THEN** the next `tlcq status` on it reports outcome `error` and deletes its scratch

#### Scenario: Laptop cancel
- **WHEN** an agent cancels a job running on the laptop
- **THEN** when `cancel` returns, no TLC process remains, its scratch is deleted, and its outcome is `cancelled`

#### Scenario: The slot after a cancel
- **WHEN** a laptop job is cancelled while another waits for the slot
- **THEN** the waiting job takes the slot, and no stale ticket holds it

#### Scenario: Rerun after a cancel
- **WHEN** a VM job ended `cancelled` and is submitted again unchanged
- **THEN** it runs anew and its outcome comes from the new run, not the old cancel marker

#### Scenario: Cancel a VM job the laptop forgot
- **WHEN** a `submit` was killed after its commit and an agent cancels that job by id
- **THEN** `cancel` asks the VM, which cancels it

#### Scenario: Pending entry seen as an older version
- **WHEN** a worktree attached offline to a queued job, and later submits a new version of that configuration
- **THEN** the queued job is superseded, and the flushed entry does not keep it alive

#### Scenario: Low disk on the laptop
- **WHEN** free disk on the laptop is below the floor
- **THEN** the job waits, and `status` says it waits for disk


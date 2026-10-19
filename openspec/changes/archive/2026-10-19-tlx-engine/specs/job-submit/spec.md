## MODIFIED Requirements

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


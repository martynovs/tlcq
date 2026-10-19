# checker-engine Specification

## Purpose
TBD - created by archiving change tlx-engine. Update Purpose after archive.
## Requirements
### Requirement: Two engines, chosen per job
Every job SHALL run on one engine: `tlx` or `tlc`. A job's engine SHALL be, first match wins, the `--engine` flag of `submit`, the `engine` of its configuration's `cfgs` entry, the `engine` of the submission config's top level, else `tlx`. tlcq SHALL NOT choose an engine for a job in any other way, and SHALL NOT run a job on another engine than its own: a tlx run that fails SHALL NOT be retried on TLC. The submit report and `status` SHALL name each job's engine.

#### Scenario: The default
- **WHEN** an agent runs `tlcq submit november NB.cfg` with no engine anywhere
- **THEN** NB.cfg runs on tlx, and the report says `tlx`

#### Scenario: A directory on TLC
- **WHEN** `november/tlcq.toml` says `engine = "tlc"` and an agent runs `tlcq submit november`
- **THEN** every configuration of the directory runs on TLC, unless its `cfgs` entry or the command line says otherwise

#### Scenario: No fallback
- **WHEN** tlx exits 150 on a spec that TLC accepts
- **THEN** the run ends with tlx's outcome, and no TLC run is started for it

### Requirement: The engine's identity and installation
tlcq SHALL pin tlx to one git rev, recorded in the repository. The laptop's install and the VM's image SHALL build tlx at that rev and record the rev beside the binary; tlcq SHALL read a machine's tlx rev from that record. A job's engine identity SHALL be `tlc` with the jar version, or `tlx` with the rev. A command SHALL be refused whole, exit 2, when any of its jobs uses an engine that is missing on the laptop (no binary or no rev record; the refusal names the install step) or whose identity differs between the laptop and the VM; a command whose jobs all use the other engine SHALL NOT be affected. Both machines SHALL build tlx from that rev with tlx's own toolchain file and lockfile. The VM SHALL keep Java and the TLC jars beside tlx. A job description stored without an engine SHALL read as TLC's, and so SHALL its run's checkpoint.

#### Scenario: tlx not installed
- **WHEN** the laptop has no tlx and an agent submits a configuration with the default engine
- **THEN** tlcq exits 2 and says to run `just install`

#### Scenario: TLC without tlx
- **WHEN** the laptop has no tlx and an agent submits with `--engine tlc` only
- **THEN** the job is placed as usual

#### Scenario: A mixed batch with a stale tlx
- **WHEN** the VM's tlx rev differs from the laptop's and a command holds one tlx job and one TLC job
- **THEN** the whole command is refused, exit 2, and says to install the same tlx rev on both sides

#### Scenario: A TLC job queued at the deploy
- **WHEN** a TLC job stored before this change is admitted after it
- **THEN** it runs on TLC, with TLC's 10-minute checkpoints

### Requirement: Running tlx
A tlx run SHALL get, with absolute paths: the spec module and `-config` of the job's configuration; `-workers` and `-maxmem` from its slot (the slot's CPU count, and the slot's memory times a configurable percentage on the VM; the laptop's settings on the laptop); `-metadir` in its attempt's scratch; `-checkpoint 10`, so that a pause lands within about 10 minutes and tlx never runs a search that writes no checkpoints; one `-I` per dependency directory, in their order; the allowlisted options. Settings that size a tlx run on the VM SHALL reach each attempt's container. It SHALL NOT get options tlx does not implement (`-fp`, `-lncheck`, `-DTLA-Library`, JVM options), nor `-fpmem`. tlx's native-code cache SHALL be shared by the VM's attempts, and SHALL NOT be in a job's directory.

#### Scenario: A job with a dependency
- **WHEN** a job of `juliet` with `extends = ["../november"]` runs on tlx
- **THEN** tlx is given `-I <job>/lib/0` and finds November's modules there

#### Scenario: A max job on tlx
- **WHEN** a max job runs on tlx
- **THEN** it is given no `-lncheck`, and checks its temporal properties after the search, as tlx always does

#### Scenario: A second run reuses native code
- **WHEN** a configuration that ran on tlx on the VM is run again in a new attempt
- **THEN** tlx finds its native build in the shared cache

### Requirement: tlx checkpoints and recovery
A running or thawed tlx attempt stopped for a pause SHALL be stopped just after tlx reports a checkpoint completed (tlx checkpoints at level boundaries, so the stop can come a level after the checkpoint fell due; tlx deletes its checkpoint once its search ends, so a pause is refused from then on); a frozen one SHALL stop at once with its last completed checkpoint (runner-ui). Its checkpoint (tlx's metadir with `checkpoint/MANIFEST`) SHALL be kept for the run's resume, as a TLC checkpoint is. The resumed attempt SHALL recover it with `-recover` and its slot's workers. Its curve SHALL take tlx's counts as they come, without adding the checkpoint's generated states again. A recovery tlx refuses SHALL end the attempt as its exit status says.

#### Scenario: Resuming in the other slot
- **WHEN** a tlx job paused in the E slot is resumed while only the P slot is free
- **THEN** it recovers in the P slot with that slot's workers, and its progress goes on from the checkpoint's counts

#### Scenario: A checkpoint from another binary
- **WHEN** the image was rebuilt between a tlx job's pause and its resume, and tlx refuses the checkpoint with exit 255
- **THEN** the run ends `error` with tlx's reason

### Requirement: Reading tlx's output
tlcq SHALL read tlx's log with the same rules as TLC's: progress lines, checkpoint and recovery lines, the final counts, the depth, the `Error:` lines and the temporal-check lines. tlx's own lines (`compile:`, `Optimizations:`) SHALL NOT change any reading. The fingerprint function SHALL be read from TLC's log only. The reason of a tlx `error` SHALL be `tlx panicked` for exit 101, and for exit 255 tlx's last `Error:`, `liveness needs more memory` or `The search stopped:` line.

#### Scenario: Progress from tlx
- **WHEN** a tlx run prints `Progress(12) at 2026-10-19 12:00:00: 1,200,000 states generated (600,000 s/min), 400,000 distinct states found (200,000 ds/min), 90,000 states left on queue.`
- **THEN** `status` reports 1,200,000 generated, 400,000 distinct and 90,000 on the queue

#### Scenario: Liveness out of memory
- **WHEN** tlx exits 255 after `liveness needs more memory: …`
- **THEN** the outcome is `error`, its reason is that line, and it is not cached

### Requirement: Guidance for choosing the engine
`tlcq submit --help` and the README SHALL say when to choose TLC: a `must fail` on a temporal property (tlx checks liveness only after the full search, TLC stops at the violation), a spec tlx refuses, and a result suspected wrong. When a tlx job's configuration says `must fail <P>` and lists `P` under `PROPERTY` or `PROPERTIES`, the submit report SHALL add a hint naming the configuration and `--engine tlc`; the hint SHALL NOT change the job. When a tlx run exits 2, 150 or 101, its result SHALL carry a hint that tlx refused it, with its first `Error:` line, and that `--engine tlc` runs it on TLC; `status` SHALL show it. A hint given while the tlx job is queued or running SHALL suggest `--engine tlc --replace`.

#### Scenario: A temporal bug variant on tlx
- **WHEN** a configuration says `must fail LivenessD`, lists `LivenessD` under `PROPERTY`, and is submitted with the default engine
- **THEN** it runs on tlx, and the report hints that `--engine tlc --replace` stops at the violation

#### Scenario: Following the hint
- **WHEN** the agent then runs `tlcq submit november NB_08_live.cfg --engine tlc` while the tlx job runs
- **THEN** the command is refused as for any running older version, and with `--replace` the tlx job is cancelled and the TLC job placed

#### Scenario: An invariant bug variant
- **WHEN** a configuration says `must fail SafetyB` and lists it under `INVARIANT`
- **THEN** the report gives no hint

#### Scenario: tlx refuses a spec
- **WHEN** a tlx run exits 150 on a module that TLC accepts
- **THEN** its outcome is `unexpected`, and `status` shows tlx's first `Error:` line and the hint to resubmit with `--engine tlc`

### Requirement: TLC checkpoints for a pause
A TLC run on the VM SHALL get `-checkpoint 10`. A running or thawed TLC attempt stopped for a pause SHALL be stopped just after TLC reports a checkpoint completed, a frozen one at once with its last completed checkpoint (runner-ui); its checkpoint SHALL be kept for the run's resume, and the resumed attempt SHALL recover it with `-recover`, the checkpoint's workers and its fingerprint function, in whichever slot it gets.

#### Scenario: A TLC job paused and resumed
- **WHEN** a max TLC job is paused in the P slot and resumed while only the E slot is free
- **THEN** it recovers its checkpoint in the E slot, and its counters go on from the checkpoint's


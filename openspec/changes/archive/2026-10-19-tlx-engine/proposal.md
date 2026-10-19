## Why

TLC is the only checker tlcq runs, and a configuration's wall time is what decides whether it fits the laptop's 5-minute slot or waits on the VM. tlx (github.com/tlaplus-x/tlx), a Rust re-implementation of TLC that compiles each model to native code, agreed with TLC on all 33 configurations tried from a real project (same exit codes, identical generated and distinct counts on every passing one, the same violated name on every `must fail`) and ran them 3.5× faster overall on the same laptop with 8 workers each, 2.3–23× per passing configuration. The exception is a `must fail` on a temporal property: tlx checks liveness only after the whole search, so it was 8–13× slower on the four large ones; that project has 51 such configurations of 498.

## What Changes

- tlx becomes the default engine; TLC stays, selectable per job. `tlcq submit --engine tlc|tlx`, and an `engine` key in `tlcq.toml` (directory level, and per `cfgs` entry); the flag wins. tlcq never picks for the agent: no automatic routing, no automatic retry.
- tlcq guides the choice: `submit --help` and the README say when TLC is the better engine; `submit` prints a hint when a configuration that must fail a temporal property goes to tlx; a run tlx refuses carries tlx's reason and a hint to resubmit with `--engine tlc` (exit 2 or 101 ends `error`; exit 150 ends `unexpected`, as it does for TLC).
- Configurations named on the command line keep the per-configuration fields of their `cfgs` entry (`engine`, and also `timeout`, `must_fail_only`, `expect`, `tlc_opts`). Today naming a configuration drops its entry, so a per-configuration `engine` would be lost exactly when a configuration is submitted by name.
- Running a configuration on the other engine counts as a new version of it: a queued one is superseded, and a running one refuses the submit unless `--replace` is given.
- **BREAKING** The engine is part of a job's id: `engine tlx <rev>` or `engine tlc <jar version>`. Every id changes, so every cached result and every history record keyed by the old ids misses once.
- tlx is pinned to one git rev in this repository. `just install` and the image build exactly that rev and record it next to the binary; the VM's preflight compares the laptop's engine identity with its own as it compares jar versions today.
- tlx runs with its own options: `-I` per dependency directory, `-maxmem` from the slot's memory, its own checkpoint layout, and on recovery the new slot's worker count, so a lane restart really changes the worker count. TLC's JVM flags, `-fp`, `-lncheck` and `-fpmem` are never passed to it.
- A configuration with no tlx history takes its placement (laptop or VM) from its TLC history, as an upper bound; estimates of time left use only the engine's own curves.
- The image ships tlx, a C compiler for tlx's native builds and a volume for its build cache, beside Java and the TLC jars.

## Capabilities

### New Capabilities
- `checker-engine`: the two engines, choosing one per job, the guidance hints, each engine's invocation, identity and installation, and what tlcq reads from each one's output.

### Modified Capabilities
These specs are still in the unarchived `tlc-queue` change; their deltas apply once it is archived.
- `job-submit`: the job id carries the engine identity instead of the jar version; the submission config gains the `engine` key, and named configurations keep their entry's fields; the duration history is kept per engine, and falls back to TLC's history for placement; a job on the other engine is an older version of the same configuration.
- `queue-runner`: a job's workers and memory follow its share in the engine's own terms; the verdict comes from the engine's exit status, which tlx shares with TLC.

## Impact

- Code: `runner/exec.rs` (the argument builder), `id.rs`, `options.rs`, `cli.rs`, `config.rs`, `history.rs`, `estimates.rs`, `curve.rs` (recovery counts), `tlc.rs` (exit 101 and 255, the fingerprint line), the checkpoint handling in `runner/exec.rs` and `runner/store.rs`, `lanes.rs` (workers on a restart), `laptop.rs`, `runner/state.rs` and `plan.rs` (the preflight's engine check), the submit report and `status`.
- Build and deployment: the `docker/Dockerfile` builds tlx at the pinned rev and adds `cc`, with tlx's native-code cache on the state volume; `just install` builds and installs tlx on the laptop; a pinned rev file in the repository.
- Data: every job id changes; the laptop cache and history start over per engine, except that TLC history still guides placement. Records stored before the change (job descriptions, checkpoints, history) read as TLC's, so jobs queued or running at the deploy finish on TLC.
- Dependency: tlx is version 0.1.0, created 2026-10-11, with no release binaries; it is built from source. Its configurations of hundreds of millions of states (disk spill, checkpoint and recovery) are untested and will be found out in production, with `--engine tlc` as the way back.

## 1. The engine and its identity

- [x] 1.1 An `Engine` (`tlx`, `tlc`, default `tlx`): `engine` at the top level and per `cfgs` entry of the submission config, `--engine` on `submit` winning over both, an unknown value refused with exit 2; carried in `JobInfo` to the VM and both supervisors; shown in the submit report and `status` (E1). Tests: config layering, the flag over a `cfgs` entry, the refusal.
- [x] 1.2 Named configurations keep their entry: a configuration named on the command line takes the per-configuration fields of the `cfgs` entry of that name in the lower layers (`Config::under`, `submit_cmd.rs`) (E1). Tests: `submit november NB_08_live.cfg` keeps the entry's `engine` and `timeout`; a name with no entry gets the top-level defaults.
- [x] 1.3 Stored records read as TLC: `engine` on `JobInfo` with a serde default of `tlc`; a checkpoint takes its run's engine (E1). Tests: a `job.json` written before the change loads as a TLC job; a TLC checkpoint recovers with TLC (the existing recovery tests).
- [x] 1.4 `tlx.rev` at the repository root; each machine's tlx binary and rev record (`$XDG_DATA_HOME/tlcq/tlx/` on the laptop, `/opt/tlcq/tlx/` in the image, overridable like `TLCQ_JARS`); a missing binary or record refuses tlx jobs only, exit 2, naming `just install` (E2).
- [x] 1.5 The manifest's identity line, `engine tlc <jar version>` or `engine tlx <rev>`, replacing `jar <version>` (`id.rs`, the laptop's and the VM's id computation, the VM's recompute in `commit.rs`); the preflight reporting both identities; `JarMismatch`, today a whole-command refusal (`plan.rs:215`), becoming per engine: the command refused whole when any of its jobs uses a mismatched or missing engine, unaffected when all its jobs use the other one (`runner/state.rs`, `plan.rs`, `submit.rs`). Tests: two engines give two ids; a stale rev refuses a tlx-only and a mixed command, not a TLC-only one; stale jars do not refuse a tlx-only one.

## 2. Running tlx

- [x] 2.1 `tlx_args` beside `tlc_args`, and `Machine::command` choosing by engine: `check`, the spec and `-config`, `-workers`, `-maxmem` (VM: slot memory × a new `tlx_mem_percent` setting, default 80; laptop: `--heap-mb`), `-metadir`, `-checkpoint 30`, `-I` per `lib/<n>` in order, the allowlisted options; never `-fp`, `-lncheck`, `-fpmem`, JVM flags (E3). The supervisors (`runner/exec.rs`, `supervise.rs`) run the tlx binary for tlx jobs, logging to `tlc.log` as before. The runner forwards `TLCQ_TLX_MEM_PERCENT` and the tlx path to attempt containers with the other `TLCQ_*` settings (`run.rs`, `runner/docker.rs`). Tests: the argument list of a short, a max and a dependency job; the forwarded settings.
- [x] 2.2 tlx checkpoints (E4): `checkpoint_in` finding TLC's `queue.chkpt` run directory or tlx's `checkpoint/MANIFEST` metadir; `keep_checkpoint` moving either; the `Checkpoint` record's engine; recovery with `-recover <dir>` and the new share's workers, no `-fp`; the curve recorder adding no checkpoint generated count for tlx. Tests: a resize from 70% to 100% recovers with the 100% share's workers; counts after a tlx recovery are not counted twice.
- [x] 2.3 Reading tlx's output (E5): the fingerprint line read for TLC only; the reason for exit 101 (`tlx panicked`) and exit 255 (the last `Error:`, `liveness needs more memory`, `The search stopped:`). Fixtures under `tests/fixtures/tlx/` from the comparison logs (`tmp/apples/*.tlx.log`): a pass, an invariant `must fail`, a temporal `must fail`, a liveness pass; the parser and the verdict pinned on each.

## 3. History and estimates

- [x] 3.1 `history::Keys` gains `engine`, defaulting to `tlc` when a record has none; the expectation lookup over the job's own engine's records first, then TLC's, the source reported as from TLC; records written under the engine that ran (E7). Tests: an old history file still reads; a first tlx run is placed by TLC's record; a tlx record wins over TLC's.
- [x] 3.2 Estimates per engine: `estimates::previous` and its population, and the runner's `ui::eta`, keep only runs of the job's engine (E7). Test: a TLC run is never a tlx job's previous run.

## 4. Guidance

- [x] 4.1 The submit hint: a tlx job whose `must fail` target is listed under `PROPERTY`/`PROPERTIES` (read with `must_fail.rs`'s cfg keywords) gets the hint line, for new jobs, cache hits and attaches alike; no hint for an `INVARIANT` target or for TLC (E6). Tests for each.
- [x] 4.2 The refusal hint: a tlx run exiting 2, 150 or 101 carries `tlx refused (exit N): <first Error: line>; resubmit with --engine tlc if TLC accepts it` in its result, and `status` shows it; 150 stays `unexpected` and cached, 2 and 101 `error` (E6). Both hints say `--engine tlc --replace` while the tlx job is queued or running. Test: following the hint while the tlx job runs is refused without `--replace` and replaces it with.
- [x] 4.3 `submit --help` for `--engine` and the README (Use, and a short "Choosing the engine" section): the default, when to choose TLC, the hints.
- [x] 4.4 The runner's page names each run's engine: `engine` in the UI API's runs, and a badge beside the configuration names (`ui/src/components/cells.tsx`).

## 5. Build and deployment

- [x] 5.1 Dockerfile: a tlx build stage at `TLX_REV` from `tlx.rev`, in its own layer; the final image with `tlx` and its `rev` under `/opt/tlcq/tlx/`, a C compiler, and Java and the jars kept; `TLX_CACHE` on the state volume, which every attempt mounts (E8).
- [x] 5.2 `just install` clones tlx at the pinned rev under `./tmp/`, builds it with `cargo build --release --locked -p tlx-cli` inside the clone (tlx's toolchain and lockfile), copies the binary to `$XDG_DATA_HOME/tlcq/tlx/tlx` and writes `rev` beside it; the README's Install section names it (E8). Check: the path tlcq reads is the path the recipe writes.

## 6. Verification

- [x] 6.1 End-to-end tests in `tests/cli.rs` with a fake tlx script beside the fake `java`: a default-engine submit runs tlx with E3's arguments; `--engine tlc` runs java; the temporal `must fail` hint is printed; a missing tlx refuses only tlx jobs.
- [x] 6.2 `just clippy` with zero warnings, `just gate` green, `just fmt`, `just large-files`.
- [ ] 6.3 Live: `just install`, `just docker-pve`; resubmit a real project's configurations and check every verdict against its TLC history; run one max configuration on tlx through a lane restart and check that it recovers with the new worker count.
  - 2026-10-19: laptop check and deploy done; oscar 6/6 ok on tlx (laptop and VM). Stopped there: the deploy coincided with the loss of NC_09 (a max TLC job); that project's agent resubmits. The corpus run and the lane-restart check on tlx remain.

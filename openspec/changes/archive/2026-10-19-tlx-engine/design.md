## Context

tlcq runs TLC only, and the coupling is direct, with no checker abstraction: `runner/exec.rs` (`tlc_args`, `Machine::command`) builds a `java … tlc2.TLC` command line for both the VM supervisor and the laptop supervisor; `tlc.rs` parses TLC's log by string prefix; `id.rs` hashes the two TLC jars into every job id; `curve.rs`, `eta.rs` and `lanes.rs` read TLC's progress counters; the lane restart (D9, D10 of `tlc-queue`) recovers a TLC checkpoint (`queue.chkpt`, `-recover`, `-fp`) with the worker count it was written with.

tlx (github.com/tlaplus-x/tlx, version 0.1.0, created 2026-10-11) reads the same `.tla` and `.cfg` files, follows TLC's option names and output format, and exits with TLC's codes. Checked against tlcq's parser: `Progress(d) at YYYY-MM-DD HH:MM:SS: …`, `Error: …`, `Deadlock reached`, `never witnessed`, `Checkpointing completed`, `Recovery completed. N states examined`, `Implied-temporal checking`, `Checking … temporal properties for the complete state space` and `Model checking completed. No error has been found.` all appear as TLC prints them. It differs where tlcq passes TLC-only options: `-fp`, `-lncheck`, `-DTLA-Library` and the JVM flags are unknown to it, and `-fpmem 1.0` is an invalid value (all exit 2). It checks liveness only after the whole search (`compat.md` §5), it can recover a checkpoint with a different worker count, and its generated count after a recovery is exact.

Evidence (a real project, 33 configurations, same laptop, 8 workers each): the same verdicts, and identical counts on every passing configuration. Without the four large `must fail` liveness configurations, TLC took 205 s against tlx's 59 s; on those four, TLC took 3.7 s and tlx 29–50 s.

## Goals / Non-Goals

**Goals:**
- tlx is the default engine, and the agent can choose TLC per job.
- Each engine runs with its own options, so neither gets an option it refuses.
- A lane restart under tlx takes the new slot's worker count.
- An id, a cached result and an estimate never mix the engines; placement may use TLC's history while tlx has none.
- The agent gets enough guidance to pick the faster engine.

**Non-Goals:**
- Choosing the engine automatically, and retrying a refused tlx run on TLC.
- Running both engines and comparing their results.
- tlx's `-tlc` mode, `-progress`, `-stats`, `-coverage` or simulation.
- Changing tlx itself (periodic liveness checking would be a request upstream).
- Validating the largest (`_max`) configurations in advance: that happens in production.

## Decisions

### E1. The engine is a property of the job

There is one `Engine` (`tlx`, `tlc`), resolved per job: the `--engine` flag; else the `engine` of the configuration's `cfgs` entry; else the directory's `engine`; else `tlx`. This is the layering `tlc-queue` D4 already gives every flag, except that a command-line flag now wins over a per-configuration entry too, named or not (`Submission::with_flags`, applied after the layers resolve): what the agent types is what runs, so it can try the other engine, or another cap, without editing `tlcq.toml`. Within the files, a per-configuration field still beats a top-level key. The engine travels in `JobInfo` to the VM and the supervisors, and is shown in the submit report and `status`.

**Named configurations keep their entry.** Today configuration names on the command line replace `cfgs` whole (`Config::under`, `submit_cmd.rs`), so `tlcq submit november NB_08_live.cfg` drops `{ name = "NB_08_live.cfg", engine = "tlc" }`, and the per-configuration fallback would fail in the most common way to submit. A named configuration therefore takes its fields from the matching entry of the lower layers' `cfgs`, if there is one. This applies to every per-configuration field, which also fixes the same loss for `timeout`, `must_fail_only`, `expect` and `tlc_opts`.

**Stored records read as TLC.** `JobInfo` gains `engine` with a serde default of `tlc`, not the submit default `tlx`. The checkpoint record does not need one: a checkpoint belongs to one run, whose `job.json` names the engine. Everything stored before the deploy was TLC's: a TLC job queued or running at the deploy finishes on TLC (a max job recovers its `queue.chkpt` with TLC), and old ended runs count as TLC's for estimates. Only a submit resolves a missing engine to `tlx`.

**The other engine is a new version.** The older-version key (worktree, model directory, spec module, configuration name) does not include the engine. So a job of the same configuration on the other engine is an older version: superseded if queued, and refusing the submit if running unless `--replace` is given. An agent that follows a hint while its tlx job runs replaces it; it does not run both. Running both side by side, to compare them, is a non-goal.

*Alternatives:* a machine-wide setting (`TLCQ_ENGINE`) cannot capture a per-configuration corner case. Automatic routing of temporal `must fail` configurations to TLC was set aside: the agent decides, and tlcq informs it (E6).

### E2. Engine identity replaces the jar line

The manifest's `jar <version>` line becomes `engine tlc <jar version>` or `engine tlx <rev>`. A TLC job's id therefore changes once even though nothing it checks changed; that is accepted (see Migration).

The laptop and the VM cannot hash one tlx binary, since one is macOS arm64 and the other Linux x86_64, and tlx reports only `0.1.0`. So the identity is a git rev: the file `tlx.rev` at the repository root pins it, `just install` and the image build exactly that rev, and each writes the rev beside the binary (`$XDG_DATA_HOME/tlcq/tlx/{tlx,rev}` on the laptop, `/opt/tlcq/tlx/{tlx,rev}` in the image). tlcq reads the rev file, never `tlx.rev` from a worktree. The preflight reports both identities, the jar version and the tlx rev. Today `JarMismatch` refuses the whole command, cache hits included (`plan.rs:215`). It becomes per engine: a command is refused, whole and with exit 2, when any of its jobs uses an engine whose identity differs between the laptop and the VM, or which is missing on the laptop (no binary or no rev file; the refusal names `just install`). A command whose jobs all use the other engine is not affected, so a stale tlx never blocks a TLC-only command, nor stale jars a tlx-only one.

*Alternatives:* a per-platform table of binary hashes would also catch a locally modified build, but needs reproducible builds; the version string `0.1.0` cannot tell two commits apart.

### E3. tlx's command line

`tlx_args` sits beside `tlc_args`, and `Machine::command` picks one by engine:

```
tlx check <job>/model/<Spec>.tla -config <job>/model/job.cfg
    -workers <cores of the share> -maxmem <M>M -metadir <scratch> -checkpoint 30
    -I <job>/lib/0 -I <job>/lib/1 …  <allowlisted options>  [-recover <dir>]
```

- `-checkpoint 30` is passed explicitly, although 30 minutes is tlx's default. Without it, tlx's default mode may apply a symmetry under a runtime guard, and a guarded run writes no checkpoints at all (`autosym.rs:39`, `check.rs:138`, `usage.md` §6.3). Such a run would never print `Checkpointing completed`, so a resize would never fall due, and a failed guard restarts the search in the same log, sending the counts back to zero. With the flag, tlx applies only symmetry it proves statically, and a run and its recovery have the same identity.

- Memory: `M` is the slot's memory times a new `tlx_mem_percent` setting (default 80), beside `heap_percent` and `offheap_percent`. An attempt builds its `Machine` from its own environment, so the runner forwards `TLCQ_TLX_MEM_PERCENT` and the tlx path to each attempt container with the other `TLCQ_*` settings (`run.rs`, `runner/docker.rs`). `-maxmem` is a soft limit over what tlx counts, so the gap to the container's `--memory` covers the rest. On the laptop, `--heap-mb` is tlx's `-maxmem`.
- Not passed: `-lncheck final` (tlx checks liveness at the end anyway), `-fpmem`, `-fp`, the JVM flags, `-noGenerateSpecTE`.
- The allowlist (`-deadlock`) is valid for both engines and is unchanged.
- tlx's native builds go to `TLX_CACHE=/state/tlx-cache`, set in the image: the state volume, which every attempt container already mounts, so no volume of its own is needed. On the laptop, tlx's default `~/.cache/tlx`.
- The supervisor runs the tlx binary instead of `java`, and the log file stays `tlc.log`, so `fetch`, the UI and the parsers keep their paths.

### E4. Checkpoints and lane restarts under tlx

With `-metadir <scratch>`, tlx writes `<scratch>/checkpoint/MANIFEST`. `checkpoint_in` finds either layout: TLC's run directory holding `queue.chkpt`, or tlx's metadir holding `checkpoint/MANIFEST`. `keep_checkpoint` moves the tlx metadir into `checkpoints/<id>/<run>/tlc` as it does TLC's run directory, and reads a fingerprint function only from a TLC layout.

On recovery, tlx gets `-recover <dir>` and the *new* share's worker count, with no `-fp`. The curve recorder does not carry the generated count forward (`Recorder::recovering`) for tlx, since tlx's counts after a recovery already include the checkpoint's. TLC's path is unchanged.

tlx checkpoints only at BFS level boundaries, and not at all during its liveness check after the search. A resize therefore waits for the end of the level after the checkpoint falls due, and cannot happen during that liveness phase; TLC pauses its workers as soon as a checkpoint is due. tlcq waits as it does today (the job keeps its slot, and the waiting job waits).

tlx refuses a checkpoint written by another binary (exit 255, naming what differs). That ends the attempt `error` under the existing verdict table. The tlx build is its own image layer, keyed by the rev, so a rebuild for a tlcq change does not rebuild tlx.

### E5. Reading tlx's output

`tlc.rs` stays the one parser; the lines it matches are the same in both engines. tlx's `compile:` lines match nothing, so they are ignored. Engine-specific handling:
- The fingerprint function (`Running … with fp N`) is read for TLC only; tlx always prints `fp 0`.
- The reason of an `error`: exit 101 is "tlx panicked"; exit 255 takes the last `Error:` line or tlx's `liveness needs more memory` / `The search stopped:` line, beside TLC's `OutOfMemoryError`.
- The exit-status table (0 and 10–14 to the verdict; 75–77 and 150–151 `unexpected`; anything else `error`) is the same for both engines.

### E6. Guidance

- `submit --help` and the README name the cases for `--engine tlc`: a `must fail` on a temporal property of a large state space (TLC stops at the first violation, tlx explores everything first); tlx refusing the spec; a result suspected to be a tlx bug.
- A hint at submit: when a tlx job's `must fail` target is listed under `PROPERTY`/`PROPERTIES` in its configuration (read with `must_fail.rs`'s cfg keywords), the report adds `hint: <cfg> must fail a temporal property; tlx checks liveness after the full search, --engine tlc stops at the violation`. It is printed for cache hits and attaches too, and changes nothing about placement.
- A hint on refusal: a tlx run that exits 2, 150 or 101 carries in its result `tlx refused (exit N): <first Error: line>; resubmit with --engine tlc if TLC accepts it`. `status` shows it. A 150 stays `unexpected` and cached, as for TLC: the same text and the same rev give the same refusal. Exits 2 and 101 are `error`, uncached, as today.
- Both hints say `--engine tlc --replace` when the tlx job is still queued or running, since the TLC run is a new version of that configuration (E1).

### E7. History and estimates per engine

`history::Keys` gains `engine`; a record written before this change has none, and reads as `tlc` (a serde default). The expectation lookup (exact, configuration, name; D5) first runs over records of the job's own engine, and, if none matches, over TLC's records, with the source shown as "from TLC". tlx is meant to be faster, so TLC's expectation is an upper bound for it; one that turns out slower on tlx still hits the laptop's 5-minute cap and `--vm-on-timeout`. Lower bounds and lane decisions are recorded under the engine that ran.

Estimates never cross engines: `estimates::previous` and the population in `estimates::eta`, and the runner's `ui::eta`, keep only runs of the job's engine. With no previous tlx run, a first-run estimate is used.

### E8. Build and deployment

- `tlx.rev` holds one full commit hash; to bump it, edit the file, then rebuild the image and reinstall.
- Dockerfile: a build stage of its own clones tlaplus-x/tlx at `TLX_REV` (a build argument read from `tlx.rev`) and runs `cargo build --release -p tlx-cli`, its rustup honouring tlx's own `rust-toolchain.toml` (1.93.0); the final image copies `tlx` and writes `rev` to `/opt/tlcq/tlx/`, and installs a C compiler for tlx's native builds. Java and the jars stay, for `--engine tlc`.
- The image sets `TLX_CACHE=/state/tlx-cache` and `CC=gcc`; compose is unchanged.
- `just install` builds tlx the way the image does: a clone at the pinned rev under `./tmp/`, then `cargo build --release --locked -p tlx-cli` *inside the clone*, so rustup uses tlx's `rust-toolchain.toml` and cargo its `Cargo.lock`. It copies `target/release/tlx` to `$XDG_DATA_HOME/tlcq/tlx/tlx` and writes `rev` beside it. `cargo install --git` is not used: run from tlcq's directory, it would pick tlcq's `stable` toolchain and resolve tlx's dependencies afresh, so two builds reporting the same rev could differ. It is a cold Rust build once per rev.

## Risks / Trade-offs

- [tlx is 0.1.0, eight days old, single-source] → TLC stays in the image and on the laptop; `--engine tlc` or `engine = "tlc"` per configuration is the way back; the rev is pinned, never floating.
- [Temporal `must fail` configurations are 8–13× slower on tlx; the project measured has 51 of them] → the submit hint (E6). The agent chooses; no automatic routing.
- [tlx's CommunityModules are its own snapshot (2026-09-12), not our jar] → a semantic difference in a community operator would show as a verdict difference against TLC; the rev pin keeps it stable; `-I` with our modules is possible later if one turns up.
- [tlx's `-maxmem` is soft; the container limit is hard] → the 80% default leaves room; an out-of-memory kill ends `error`, uncached, as today; tune `tlx_mem_percent` from production.
- [Liveness needs memory for the whole behavior graph; past `-maxmem` tlx exits 255] → reported as `error` with tlx's reason; such a configuration can run on TLC.
- [tlx checkpoints only at level boundaries and never during its liveness check] → a resize of a max tlx job can wait a level or the whole liveness phase longer than under TLC, and the job waiting for the side slot waits with it; accepted, and to be watched in production.
- [A rebuild of the same rev yields another binary, and tlx refuses its checkpoint] → the tlx layer is cached by rev; a refused recovery ends the attempt `error`, and the agent resubmits.
- [Automatic symmetry in tlx's default mode can change state counts] → counts are never compared across engines: ids, caches and estimates are per engine.
- [tlx compiles C generated from the spec and loads it] → the same trust as running TLC on the spec; the specs come from our own agents; each attempt runs in its own container.
- [Every job id changes] → caches miss once; TLC history still guides placement; old records keep working.

## Migration Plan

1. Merge with `tlx.rev` pinned to bec5985 (the rev the comparison used).
2. Let the laptop's own jobs end first: one placed by the old binary keeps an id in the old manifest form, which the new VM refuses on a `--vm-on-timeout` move. Then `just install` on the laptop (TLC jars stay where they are), and `just docker-pve` right after. Between the two, a laptop newer than the runner is refused at the preflight (`RunnerOutdated`: an old runner's reply has no `tlx_match`) instead of failing at the commit.
3. The first submit of every configuration misses the caches. Placement uses TLC history; estimates start without a previous run.
4. Rollback: first let the VM's queued and running tlx jobs end or cancel them, since the previous binary ignores the `engine` field and would run them on TLC under tlx ids. Then redeploy the previous image and binary. TLC ids return to the jar line, and the old caches and history still match them.

## Open Questions

- The `tlx_mem_percent` default (80) is a guess; it is set from the first `_max` runs.
- Whether tlx's `-progress` (every 10 s instead of every minute) would make estimates earlier and better; not in this change.

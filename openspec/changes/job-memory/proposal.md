## Why

A VM job gets whatever memory its slot has, and nothing lets an agent ask for more. RP_takeover_live_install_max (6950449c9c0d) searched for 1h46m and then failed in liveness: tlx's product needed about 48 GB against a 43.5 GB budget (80% of a P slot's share, with E busy). With only the E slot running, it would have had about 97 GB. tlx's own advice, "rerun with -maxmem 48064 or more", cannot be followed through tlcq: no flag or `tlcq.toml` field reaches the attempt's memory, and only a human closing the P slot from the page gives a job the whole VM.

## What Changes

- **A memory request per job.**
  - A `memory` field in a configuration's `tlcq.toml` entry (`{ name = "X_max.cfg", memory = "64G" }`), a top-level `memory` key, and a `--memory 64G` flag on `submit`. Flags win, as for the other per-configuration fields.
  - The request is the least memory, in MB, that each of the job's VM attempts runs with.
  - It is one of the run's flags, kept from its first submitter like `--timeout`: an attach with another request is warned, and the run keeps the first.
  - It is not part of the job's id (a resource setting, like the time caps). A job that ended `error` (as "liveness needs more memory" does) already runs anew when committed again, so resubmitting with `--memory` reruns it.
- **Admission honours the request.**
  - A run with a request starts only in a slot whose memory (the slot's share, cut to what is free, as today) reaches it. Short of it, the run waits like any run short of memory, first in, first out.
  - A request above one slot's share with both slots open is a **whole-VM** run. It starts only in E, only while no job holds P, with the memory both slots would share, as with the P slot closed.
  - While a whole-VM run is live and not paused, admission starts and thaws nothing in P.
- **Refusals.**
  - The VM refuses at commit a request above what it can ever give: all its memory less the reserve.
  - `submit` refuses `--local` (or a laptop placement) for a job with a request, exit 2. A memory request is a VM matter, and the laptop has no slots to give up.
- **A hint after running out of memory.**
  - A run that ended with tlx's "liveness needs more memory: … needs about N MiB" gets a status hint: `needs more memory: resubmit with --memory <G>`. The `<G>` is N scaled up by tlx's share of the attempt's memory and rounded up to a whole GB.
  - A run killed for memory (out of memory, or SIGKILL with nothing to say) gets `needs more memory than its <M> MB`.
- **Docs:** the README's Configuration section, the submit flags, and the page's slot view (a whole-VM run shows P held).

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `job-submit`: the TOML config and the submit flags gain `memory`; the job's id stays free of it; a request with `--local` is refused; a run keeps its first submitter's request among its flags.
- `queue-runner`: the "Two slots" admission rules gain per-run memory requests and the whole-VM run that holds P; commit refuses a request the VM can never give; an attempt records the memory it started with.
- `job-status`: the status line of a run that ran out of memory carries the resubmit hint.
- `runner-ui`: a whole-VM run's hold on the P slot is shown.

## Impact

- **Client:** `config_file.rs`/`options.rs` (the `memory` key and entry field, size parsing), `cli.rs` (`--memory`), `submit.rs` (the attach warning, the `--local` refusal), `proto.rs` (`JobInfo`/`Flags` gain `memory_mb`; older descriptions read as none), `report.rs`/`status.rs` (the hint).
- **Runner:**
  - `runner/admit.rs`: `pick` checks the request; a whole-VM start, and P held while such a run is live.
  - `runner/state.rs`: `Attempt` records its memory and whether it holds the whole VM.
  - `runner/apply.rs` (or the commit verb): the refusal.
  - `runner/exec.rs`: parse tlx's "needs about N MiB" into the result.
  - `runner/ui.rs`, `ui/`.
- **Model and docs:** `tla/runner/Runner.tla` (a run that holds both slots), re-checked with `tlcq submit --local`; the specs above; the README.
- **Compatibility:** the wire gains optional fields only. A runner or laptop that predates them reads no request.

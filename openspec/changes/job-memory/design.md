## Context

A VM attempt's memory is decided by admission, at start (`Launch::memory`, `runner/memory.rs`). With both slots open it is the slot's equal share of the VM less the reserve (all of it with only the E slot open), cut to what is free less the room the running attempts may still grow into; below `MIN_MB` (4 GB) the run waits. tlx gets `tlx_mem_percent` (80%) of that as `-maxmem`, and TLC its heap and off-heap shares. Nothing per job reaches this. The only way to give one job the whole VM is a human closing the P slot on the page.

Things already in place that the change leans on:
- A run keeps its first submitter's `Flags` (`timeout`, `vm-on-timeout`), and an attach with other flags is warned.
- A job's id leaves out resource settings and time caps (job-submit).
- A run that ended `error` runs anew when committed again.
- A run short of memory waits at the head of the queue, and its slot runs a frozen job on spare capacity meanwhile (`short_of_memory`).
- The TLA+ runner model (`tla/runner/Runner.tla`) has the slots, `Open`/`Free`, freezes and the P toggle, with memory abstracted ("a run short of it waits like one with no slot").

## Goals / Non-Goals

**Goals:**
- An agent that reads "needs more memory" can resubmit with a request that makes the next run fit, with no human involved.
- A request that one slot can serve costs the other slot nothing.
- A request that needs the whole VM gets it the way closing the P slot would, and only while that run is live.

**Non-Goals:**
- Changing the default split: slots stay equal shares, and requests are opt-in.
- Laptop memory: the laptop's TLC heap and off-heap settings stay its own.
- Growing a running attempt's memory: Docker's cap is set at start.
- Choosing tlx's `-maxmem` share per job: it stays `tlx_mem_percent` of the attempt's memory.

## Decisions

### M1. The request is a run flag, not part of the id
`memory_mb: Option<u64>` joins `Flags` and `JobInfo`. As with `timeout`, an attach keeps the first submitter's value and is warned.

The id stays as it is: the job-submit spec already keeps resource settings out of it, and the rerun path needs no new id. The failing case ends `error`, and committing the same id again starts a new run with the new committer's flags.

*Alternative:* hashing the request into the id. Rejected: it breaks a stated invariant (one id per model text, configuration and engine), and splits cached verdicts by memory.

### M2. One slot or the whole VM, decided by the slot's share
A run whose request `R` fits a slot's share (`R ≤ share`, half the VM less the reserve with both slots open) is placed as today. `pick` takes it only if `memory(slot) ≥ R`. If it is short, it blocks the queue first in, first out, and the slot runs a frozen job on spare capacity, as for any run short of memory.

A run with `R > share` is a **whole-VM** run. It can start only in E, only while no job holds P, and with `memory(E, alone = true) ≥ R`. Its attempt records `whole = true` and the memory it started with (`Attempt::memory_mb`, also kept for the hint, M6).

*Alternative:* a whole-VM run could take P as well as E (both CPU sets). Rejected: the slot's workers stay its CPU list, and a 32-worker tlx is slower than 16 (pinned-slots evidence). What the run needs is memory, not cores.

### M3. A whole-VM run holds P while it is live and not paused
While some live attempt with `whole = true` has not been paused, P counts as closed for admission: `free_slots` leaves it out, nothing starts or thaws there, and a thawed job in P is frozen again at once, as when a human closes it.

The hold lasts through the run's freezes. A frozen whole-VM run keeps the memory it uses, and its limit is the whole VM's. A job let into P meanwhile could leave it no room when it thaws.

The hold ends when the run ends or pauses. A pause frees all of its memory.

The page's P toggle is unchanged: a human can still close P, and reopening it doesn't lift a whole-VM hold.

*Alternative:* the hold covers only the running time, freed while frozen. Rejected: thawing could then over-commit memory. The runner refuses to over-commit everywhere else.

### M4. Getting to a whole-VM start
A whole-VM run at the head of the queue, with P held by a running job, waits: it is short of memory in the existing sense. Meanwhile:
- E may run a frozen job on spare capacity, frozen again once the whole-VM run can start;
- runs behind it do not start, first in, first out;
- P, once free, starts nothing while the head waits for it.

Without that last rule a stream of small runs could keep P busy for ever.

The waiting run says why on the page and in status: "waits for the whole VM".

*Alternative:* let smaller runs overtake it. Rejected: it can starve the run, and the queue is first in, first out throughout.

### M5. Refusals
The VM refuses at commit a request above `memory_mb − reserve_mb`, the most it can ever give, with a reason naming that limit. It is refused like any other commit refusal, exit 2. The client can't check this itself, because it doesn't know the VM's memory.

`submit` refuses `--local`, and any job the plan would place on the laptop, when it carries a request (exit 2: "--memory is for VM runs"). A short job with a request goes to the VM's queue instead of the laptop's 5-minute slot.

*Alternative:* ignoring the request on the laptop. Rejected: silently dropping what an agent asked for hides the very failure the request is meant to fix.

### M6. The hint
The supervisor already reads the engine's errors. It now also reads tlx's `liveness needs more memory: … needs about N MiB` and writes the hint into the result's `note`, which the status line already prints after "; " (`runner/memory.rs`: `needed_hint`, `killed_hint`).

The status line then adds `needs more memory: resubmit with --memory <G>`. `<G>` is `N × 100 / tlx_mem_percent`, rounded up to a whole GB, so the attempt's tlx budget would cover N.

A run that ended "out of memory", or killed by signal 9 with nothing to say, gets `needs more memory than its <M> MB` from `Attempt::memory_mb`. That number is a floor, not a size.

The VM computes the hint's number, since only it knows `tlx_mem_percent`. The client only prints it.

### M7. Size syntax
`memory` and `--memory` take an integer with an optional `M`, `G` or `T` suffix (powers of 1024; `64G` = 65536 MB). A bare integer is MB. Less than `MIN_MB` is refused at parse time (exit 2), since no attempt starts below it anyway.

### M8. Model
`Runner.tla` gains a constant set `Whole ⊆ Ids` of whole-VM jobs:
- such a job's `FreshOK`/`ResumeOK` require `s = "e" ∧ Free("p")`;
- `Open("p")` also requires that no attempt of a `Whole` job is live and not paused;
- refreezing a thawed job in P follows the existing P-closed path.

New invariant `WholeAlone`: an attempt of a `Whole` job holding E implies no attempt holds P. A must-fail config drops the `Free("p")` conjunct (`WholeNoWait`) and expects `WholeAlone` to fail. Both are re-checked with `tlcq submit --local`.

## Risks / Trade-offs

- **A whole-VM run idles P's cores for its whole run.** That is the cost of its memory, and the page shows P held so a human can see why. → The hint suggests the least memory that fits, so a request that one slot can serve stays a one-slot run.
- **Agents may over-ask "just in case" and serialize the VM.** → The hint gives a number derived from tlx's own estimate, and the README advises asking only after a failure.
- **tlx's estimate says "so far".** The product may grow past N, so a rerun could fail again with a bigger N. → The hint rounds up to whole GB, and the second failure's hint is larger. No one can know the final size before running, tlx included.
- **The first submitter's request wins on attach.** A second agent asking for more is only warned. → Same rule and warning as `--timeout`. The second agent can resubmit once the run ends `error`.

## Migration Plan

All the new fields are optional on the wire and in `state.json`. An older laptop sends no request, and an older runner ignores one (unknown fields are dropped by serde defaults). Deploy the runner and laptop together, as usual. Rolling back drops the requests and any held P with them.

## Open Questions

None blocking. One to watch after use: whether whole-VM runs should be allowed to overtake a P job that is near its end (by ETA). This proposal keeps strict first in, first out.

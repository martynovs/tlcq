## 1. The model first

- [x] 1.1 `tla/runner/Runner.tla` (M8): a constant `Whole ⊆ Ids`. A `Whole` job's `FreshOK`/`ResumeOK` require `s = "e" ∧ Free("p")`. While a `Whole` job has an attempt live and not paused, `Open("p")` is false: nothing starts or thaws in P, and a thawed job in P is frozen again as on the P toggle. While a `Whole` job waits at the head for P, P starts nothing.
- [x] 1.2 New invariant `WholeAlone`: a `Whole` job's attempt holding E implies no attempt holds P. A must-fail `RN_wholenowait.cfg` drops the `Free("p")` conjunct. Model-check `RN`, `RN2`, `RN3` and every `RN_<bug>.cfg` with `Whole` set in one config and empty in the rest, through `tlcq submit --local` (small configs) and the VM (large). Update the module's header.

## 2. The request on the client

- [x] 2.1 Size parsing (M7): an integer with an optional `M`/`G`/`T` suffix, in powers of 1024, a bare integer is MB; refused below `MIN_MB` (exit 2). Unit tests for each suffix, a bad size and the floor.
- [x] 2.2 `memory` as a top-level key and a per-configuration field (`config_file.rs`/`options.rs`, the layering of the other per-configuration fields), `--memory` on `submit` (`cli.rs`), the flag winning. Tests mirroring the `engine`/`timeout` layering scenarios.
- [x] 2.3 `memory_mb` in `JobInfo` and `Flags` (`proto.rs`, `#[serde(default)]`). The attach warning in `submit.rs` names it beside `timeout` and `vm-on-timeout`. The id is unchanged; add a test that two submits differing only in `--memory` get one id.
- [x] 2.4 Placement (M5): a job with a request is never placed on the laptop. `--local` with a request is refused (exit 2, "--memory is for VM runs"), and a short job with a request goes to the VM's queue. Integration test in `tests/cli.rs`.

## 3. The request on the runner

- [x] 3.1 `Attempt` gains `memory_mb` (as started) and `whole` (`runner/state.rs`, defaulted for older state). `Launch::start` records them.
- [x] 3.2 Admission (`runner/admit.rs`, M2–M4). `pick` takes a run with a request only if the slot's memory reaches it. A request above a slot's share with both slots open is a whole-VM run: it starts only in E, only while no job holds P, with `memory(E, alone = true)`. While a whole-VM run is live and not paused, `free_slots` leaves out P and P starts and thaws nothing, and a thawed job in P is frozen again at once. A whole-VM run at the head waiting for P keeps P from starting anything. Tests in `admit_tests.rs`, one per queue-runner scenario.
- [x] 3.3 Commit refuses a request above `memory_mb − reserve_mb`, naming the limit (`runner/apply.rs` or the commit verb), with a test.
- [x] 3.4 `waits_for` says "the whole VM" for a whole-VM run waiting on P (the page and `status`).

## 4. The hint

- [x] 4.1 `runner/exec.rs`/`tlc.rs`: read tlx's "liveness needs more memory: … needs about N MiB" into the result's `note` (`memory::needed_hint`), scaled to an attempt's memory (`N × 100 / tlx_mem_percent`, rounded up to a whole GB). Test with the real line from 6950449c9c0d's log.
- [x] 4.2 The status line (`report.rs`/`status.rs`): `needs more memory: resubmit with --memory <G>` for that result. For a run ended out of memory, or by signal 9 with nothing to say, `needs more memory than its <M> MB` from its last attempt's `memory_mb`. Report tests for both.

## 5. The page

- [x] 5.1 `runner/ui.rs` and `ui/`: a running attempt's memory and a run's request on its row. The P slot shown held by a whole-VM run "for its memory", with the toggle saying the hold lasts until the run ends or pauses. `just ui-check`.

## 6. Docs and close

- [x] 6.1 README: the `memory` field in the Configuration example, `--memory` with its sizes, and the advice to ask only after a "needs more memory" hint. The `tlcq submit --help` text for `--memory`.
- [ ] 6.2 Apply the delta specs: job-submit, queue-runner, job-status, runner-ui.
- [x] 6.3 `just test`, `just clippy`, `just gate`, `just fmt`, `just large-files`. Deploy only after checking `tlcq ls` for running jobs, and ask.
- [ ] 6.4 On pve: rerun RP_takeover_live_install_max with the hint's `--memory` and confirm it starts as a whole-VM run in E, holds P, and completes liveness.

## 1. Client

- [x] 1.1 The TOML submission config (`tlcq.toml`, `--config <file>|-`, layered with the command line); the job tarball and its layout; the `--must-fail-only` configuration rewrite; the manifest id and jar version; the option allowlist (D2–D4, D8).
- [x] 1.2 The expected duration: flags, suffixes, the history, its records, lookups and lower bounds, awake time, the suffix warning (D5).
- [x] 1.3 The job records, their states and refresh, the generation counter; input checks; the placement lock and VM-call deadlines; submitter entries, pending entries and the mirror; the preflight; older versions, supersede and `--replace`; routes; `commit` over the runner command's stdin, its unknown-outcome exit 3 and its `invalid` answer (D6 Records); follows (written at placement, rewritten by the answer or a later token lookup, removed on `invalid`); attach; the laptop cache (D6).
- [x] 1.4 The supervisor (`tlcq exec`; on the laptop `tlcq supervise`): running on the extracted tarball; on the laptop the slot tickets, locks, the disk floor, the 5-minute cap, timeout records, `--vm-on-timeout`, `--local`, cancel markers, orphans (D2, D7).
- [x] 1.5 The `submit` report (the skip-max listing, `--all`); `status` for an agent's current jobs from its follows (the latest per key, plus any older one still queued or running with its key's entry) or named ones; the ended record a cache hit writes; `ls`, `cancel`, `fetch`; exit codes (D1, D11).
- [x] 1.6 The ETA (D15): progress curves recorded per attempt, the estimators reading the current attempt's curve and a previous run's last attempt; the previous-run estimator (Q/N match, size and speed factors, shape fit) and the first-run one (Q/N extrapolation, population shape); the temporal-check tail; replayed against a fixture of a real project's TLC logs.

## 2. Runner

- [x] 2.1 The VM verbs, payloads on stdin, read and validated under a VM-side deadline before the admission lock: `preflight` (under the lock, writing only token tombstones: the jar check, job states with cached results and lane decisions with their probe numbers, entries, ended uncached jobs, older versions, the run each commit token reached, unsettled attempts reported as settling would); `commit` (validation and the id recomputed before the lock; then under it settling, idempotent storage with each job's token on the run it reached, pending entries, jobs with attaches, supersedes, admission with `now-or-queue`/`now-or-refuse`; the apply immune to a dropped session); `status` (as the preflight: token lookups, per-id state and entries), `cancel`, `fetch`; entry storage; tokens recorded on the run reached or the cached answer, or as not applied, and tombstoned by a lookup so a late commit aborts (D6 Tokens); the runner's start deleting only ended attempts' scratch.
- [x] 2.2 The queue on disk and `tlcq admit`: the two slots and their shares, first in, first out by first commit to the VM (an undone or failed-start run keeping its place, a failed container start ending the round; a human's reorder), the disk floor, the time caps and `--timeout` (D9).
- [x] 2.3 The runner (`tlcq runner`): one container and directory per attempt running `tlcq exec`, started, waited on and removed through the podman socket; caps enforced by the supervisor; settling an attempt (the stop hook, its outcome order and write order; every writing verb and the runner's start settling gone attempts first (undoing one without its "started" marker unless a cancel marker ends it `cancelled`; the never-started limit; attempt numbers never reused) and sweeping ended attempts' scratch; the hook deleting its scratch after the commit point, before admission; derived slot use; admission writing the attempt's record, directory and first `result.json` before starting its container); cancel, of a pending restart included; waiting again on live containers after a runner restart (D9, D10).
- [x] 2.4 A fixture test pinning TLC's exit status and output; the verdict and outcomes; `result.json`; progress parsing for status (D8, D11).
- [x] 2.5 Lane decisions in the VM supervisor: short to slow at 15 minutes; the size probe 15 minutes into the slow lane, per-lane probe times and thresholds; the max rule (at 100% a restart at 70% with a restart marker and a held slot; in the 70% slot max in place; in the side slot a restart at 70% if free, else a too-big marker and `error`); caps enforced by the supervisor; one container and directory per attempt; probe numbers recorded (D9).

- [x] 2.6 The web UI on the runner (D16): the page (slots, queue in order, running jobs' progress and ETA, ended jobs with outcome and logs, submitters), refreshing itself; cancel (canceller `ui`) and reorder (a queued run up, down or to the head; never a recorded run or a held restart), each a verb under the admission lock; the ETA estimators shared with the client (1.6).

## 3. Seams and provisioning

- [x] 3.1 `config.toml`, the state directory and the submitter label (D13); the configurable remote command, the container launcher interface, a configurable cap, poll interval and clock (D12).
- [x] 3.2 The job image (Dockerfile: Java, pinned TLC jars, the `tlcq` binary built for Linux); the runner on pve as a compose service (state volume, podman socket, `restart: always` with podman-restart for boot, traefik labels for `tlcq.pve.local`); the same jars on the laptop (D9, D13, D16).
- [x] 3.3 `tlcq.toml` in each model directory of the first project to use it; its runs routed through tlcq.

## 4. The protocol model

- [x] 4.1 Keep `tla/placement` and `tla/runner` passing: `PL.cfg`, `PL3.cfg`, `RN.cfg`, `RN2.cfg` and `RN3.cfg` hold every invariant, each `PL_<bug>.cfg` and `RN_<bug>.cfg` fails as its header says; change the model before D6, D7, D9 or D10 (D14).

- [x] 4.2 Tests for what the models leave out (D14): a commit held after its client gave up while a poll looks its token up, then released: the poll tombstones the token, the commit applies nothing, the follow ends `never sent`; a writing verb that reaches the runner before its start-up settling starts a job, then that settling runs: the live job's scratch survives, only ended attempts' scratch is deleted; a model directory holding a name the VM refuses (e.g. `Spec copy.tla`): `submit` exits 2 at its input checks, nothing superseded, nothing written (D6).

## 5. Review

- [x] 5.1 Review rounds of this change, before any code, closed by a final round reporting only high- and medium-severity defects with a concrete failure scenario; its three findings are fixed (tokens tombstoned by lookups, boot deleting only ended attempts' scratch, the VM's tarball checks run at input) and their tests are 4.2.

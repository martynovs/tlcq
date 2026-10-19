# Working in this repo

## 1. Think before coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them — don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

## 2. Simplicity first

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

## 3. Surgical changes

**Touch only what you must. Clean up only your own mess.**

- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it — don't delete it.
- Remove only imports/variables your changes made unused.

The test: every changed line should trace directly to the request.

## 4. Goal-driven execution

**Define success criteria. Loop until verified.**

- "Add validation" → "Write tests for invalid inputs, then make them pass."
- "Fix the bug" → "Write a test that reproduces it, then make it pass."
- "Refactor X" → "Ensure tests pass before and after."
- For multi-step work, state the plan up front with a verification checkpoint per step.

---

## Rust workflow

**Run tests with `cargo nextest run` (`just test`), not `cargo test`.** nextest runs
the test binaries in parallel, so the suite is far faster.

**After any non-trivial change, run in order: clippy → the coverage gate → format.**
Each step gates the next; formatting is always last before staging.

**1. Clippy.** `cargo clippy --workspace --all-targets` (`just clippy`), not `cargo
check` — the workspace opts into `clippy::pedantic` plus a strict opt-in set, so plain
`check` is too lenient. Confirm zero warnings before claiming code is clean. `cargo
clippy --fix --allow-dirty` auto-applies most fixes; finish the rest by hand. Real
correctness flags (`cast_possible_truncation`, `cast_sign_loss`, …) warrant a fix;
cosmetic pedantic flags on intentional code can be `#[expect(..., reason = "...")]`'d
(the lint wall requires a reason).

**`missing_docs` is denied**, so a public item without a doc fails the build. Nothing else
can see that fault: rustdoc judges the docs that exist, never one that is absent. It matters
most when a doc block is *moved* — a function drifting away from its own prose leaves the
block merged onto its neighbour, which compiles and reads as a single correct doc. Note the
lint sees only public items, so a `pub(crate)` or private one can lose its doc unnoticed.
Where a module documents fields and variants on the *type* instead of one by one, scope an
`#![allow(missing_docs, reason = "...")]` to that module rather than paying the lint off with
`/// The name.` restatements.

**2. Gate.** `just gate` runs three checks — two off one instrumented run, then the docs:
- **CRAP** (`cyclomatic² × (1 − coverage)³ + cyclomatic`; threshold 30, in
  `.cargo-crap.toml`) against the committed baseline `.cargo-crap.json`. Fix a failure
  by adding test coverage or reducing cyclomatic complexity — don't blindly
  `just crap-baseline` to silence it.
- **Per-function coverage floor: every function ≥90% line coverage.** Extract untestable
  I/O so the logic is reachable; don't lower the bar.

  A CLI's `main` is covered end-to-end by the integration tests that spawn the built
  binary (`tests/cli.rs`, via `env!("CARGO_BIN_EXE_<name>")` + `std::process::Command`).
  Unit tests cover the pure logic and the clap parse surface. `main.rs` is **not**
  excluded from the gate — keep `main` a thin shim so a few end-to-end invocations
  clear the floor.
- **Docs build clean** (`just doc` — `cargo doc --workspace --no-deps
  --document-private-items` under `RUSTDOCFLAGS="-D warnings"`). Here because nothing
  else can see it fail: a broken intra-doc link, a `[`Name`]` pointing at a renamed
  type, a code fence rustdoc tries to compile — none of it stops a build, clippy or
  the tests. **`--document-private-items` starts as a no-op and becomes load-bearing**:
  a lone binary has no public API to narrow to, so rustdoc documents everything either
  way, but a library skips private items unless asked — and in a prose-heavy codebase
  most of the writing hangs off them. The flag is there from the start so adding a
  `lib.rs` does not silently shrink what the check covers.

**3. Format.** Once clippy and the gate are green, run `cargo fmt --all` (`just fmt`) as
the very last step. Default `rustfmt`, no `rustfmt.toml`. Don't run it while iterating —
it churns the diff.

**4. Large-files smell (when done).** `just large-files` lists hand-written `.rs` files
over 30 KB — a prompt to split, not a hard failure.

**Compact the comments before splitting the code.** These files are prose-heavy, so a file
usually crosses 30 KB because the doc grew, not because the code did. Splitting then hides
the real problem and buys a module boundary nothing asked for — check what the code alone
weighs first, and tighten the prose. Say the load-bearing thing once, in fewer words; drop
the restatement, the worked-through alternative, and the paragraph that recaps the previous
paragraph. Split only when the *code* has genuinely outgrown one file, and then only at a
seam that already exists.

### Module structure

**`lib.rs` and every `mod.rs` are import-only** — module declarations and re-exports,
nothing else. Put logic (and its tests) in a named sibling submodule and surface it
through a re-export. A binary's `main.rs` is the exception: it carries `fn main` plus the
`mod` declarations.

## Conventions

- **All scratch activity lives in `./tmp/`** (git-ignored) — logs, coverage
  artifacts, validation spikes, cloned reference repos, ad-hoc DBs, dumps. Never
  write throwaway files to `/tmp` or the repo root.
- **`tee` operation logs into `./tmp/`** — the output of things that *do* work
  (builds, test runs, the gate, migrations, long commands), before any filter, so
  the whole log survives even when you only skim a slice:
  `set -o pipefail; <cmd> 2>&1 | tee ./tmp/<name>.log | tail -50`. The
  `set -o pipefail` is mandatory — without it the pipeline returns `tee`'s exit
  code (`0`) and a failing `<cmd>` is silently masked. Give each command in a
  multi-command call its own `tee` — never one `tee` wrapping the whole block.
  This is for operation logs only: plain **reads** (`cat`, `jq`, `grep`, `ls`,
  reading a file or a value) are not `tee`'d — just read them directly.
- Task runner is `just` — run `just` (or `just --list`) to see recipes.

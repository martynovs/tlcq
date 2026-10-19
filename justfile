# cargo-crap file excludes (globs are workspace-relative). Test code is not
# subject to the CRAP/coverage gates: integration tests (`**/tests/**`), split-out
# unit-test modules (`**/*_tests.rs`), examples and benches. NOTE: `**/main.rs` is
# deliberately NOT excluded — `main` is covered end-to-end by the integration tests
# that spawn the built binary (see the seed crate's tests/cli.rs), which is exactly
# the coverage the per-function floor expects.
crap_excludes := "--exclude '**/tests/**' --exclude '**/examples/**' --exclude '**/benches/**' --exclude '**/*_tests.rs'"

# CRAP threshold read straight from .cargo-crap.toml (the same source cargo-crap
# uses), so the gate predicate can't drift from the report. Falls back to 30.
# NB: extract only the value's first integer — a stray `s/[^0-9]+//g` would
# concatenate digits from trailing comments (`threshold = 25  # was 30` → 2530).
crap_threshold := shell("v=$(sed -nE 's/^threshold[[:space:]]*=[[:space:]]*([0-9]+).*/\\1/p' .cargo-crap.toml 2>/dev/null | head -n1); echo ${v:-30}")

# Per-function line-coverage floor (percent).
cov_floor := "90"

# Shared preamble for the coverage recipes (interpolated as `{{ cov_setup }}`):
# ensure ./tmp exists, and on toolchains whose rustc lacks llvm-tools (e.g. Homebrew
# rust) point cargo-llvm-cov at a standalone llvm-profdata/llvm-cov if one is on PATH.
cov_setup := '''mkdir -p tmp
if command -v llvm-profdata >/dev/null 2>&1; then
  export LLVM_COV="$(command -v llvm-cov)"
  export LLVM_PROFDATA="$(command -v llvm-profdata)"
fi'''

# List available recipes.
default:
    @just --list

# Run the CLI, e.g. `just run --name Ada`.
run *args:
    @cargo run --quiet -- {{ args }}

# Run tests with nextest.
test *args:
    cargo nextest run --workspace {{ args }}

# Lint the workspace (pedantic + strict).
clippy:
    cargo clippy --workspace --all-targets

# Format all code (run last).
fmt:
    cargo fmt --all

# **In the gate because nothing else can see this fail.** Broken intra-doc links, a `[`Name`]`
# pointing at a type that was renamed, a code fence rustdoc tries to compile — none of it stops
# `cargo build`, `clippy` or the tests, so a doc rots silently between the times somebody happens to
# run `cargo doc` by hand.
#
# **`--document-private-items` earns its place the moment a library crate appears**, and not
# before. The seed is a single binary, where rustdoc has no public API to narrow to and documents
# everything anyway — measured, by breaking a link on a private `fn` and watching the run fail
# with the flag and without it alike. A library is the opposite: rustdoc skips private items
# unless asked, so prose hanging off private modules goes unchecked and its links rot behind a
# green gate. The flag is here from the start so that growing a `lib.rs` does not quietly narrow
# what this recipe covers.
#
# `-D warnings` rather than reading the output: rustdoc's warnings are the whole signal, and a check
# whose result has to be eyeballed is one that passes on a busy day.
#
# `[doc]` rather than letting the comment above serve: `just --list` shows only the *last*
# comment line, so a multi-paragraph rationale renders as a sentence fragment.
[doc("Check the docs build with no warnings.")]
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items

# Generate a CRAP report.
crap:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ cov_setup }}
    cargo llvm-cov --workspace --lcov --output-path tmp/lcov.info
    cargo crap --lcov tmp/lcov.info {{ crap_excludes }}

# Gate CRAP against the committed baseline.
crap-ci:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ cov_setup }}
    # Reuse an already-collected tmp/lcov.info when REUSE_LCOV is set (see `gate`).
    if [ -z "${REUSE_LCOV:-}" ]; then
        cargo llvm-cov --workspace --lcov --output-path tmp/lcov.info
    fi
    cargo crap --lcov tmp/lcov.info {{ crap_excludes }} \
        --baseline .cargo-crap.json --format json --output tmp/crap-delta.json
    BAD=$(jq --argjson t {{ crap_threshold }} \
        '[.entries[] | select(.status == "regressed" or (.status == "new" and .crap > $t))] | length' \
        tmp/crap-delta.json)
    if [ "$BAD" -gt 0 ]; then
        echo "CRAP gate FAILED: $BAD offending entries (regressed or new-over-threshold)"
        jq --argjson t {{ crap_threshold }} \
            '.entries | map(select(.status == "regressed" or (.status == "new" and .crap > $t))) | sort_by(-.crap) | .[] | {status, function, file, line, crap, cyclomatic, coverage}' \
            tmp/crap-delta.json
        exit 1
    fi
    echo "CRAP gate PASSED: no regressions, no new over-threshold functions"

# Run the CRAP + coverage gates off one instrumented run, then the docs.
gate:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ cov_setup }}
    # A FULL clean, profraw and instrumented objects both, which costs a
    # recompile and is not optional.
    #
    # Dropping only the profraw is not enough, and the way it fails is a trap.
    # `llvm-cov report` merges every instrumented object it finds, including
    # ones an earlier build left behind — and those carry the LINE TABLE they
    # were compiled with. After any edit that shifts lines, coverage is
    # attributed to where code USED to be, so the report blames comments,
    # blank lines, and `impl` headers, and functions nobody touched drop below
    # the floor. The tell is one function reported at several different line
    # numbers in tmp/lcov.info:
    #
    #     grep -E '^FN:[0-9]+,.*<name>$' tmp/lcov.info | cut -d, -f1 | sort -u
    #
    # More than one line there means the report is measuring a mix of builds.
    # (`clean --workspace --profraw-only` also warns the flags conflict.)
    cargo llvm-cov clean --workspace
    cargo llvm-cov --workspace --no-report
    cargo llvm-cov report --lcov --output-path tmp/lcov.info
    REUSE_LCOV=1 just crap-ci
    REUSE_LCOV=1 just coverage
    # Last, and outside the instrumented run: rustdoc shares nothing with the coverage build, so
    # this is a plain `cargo doc` that would otherwise never be run except by hand.
    just doc

# Regenerate the committed CRAP baseline.
crap-baseline:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ cov_setup }}
    cargo llvm-cov --workspace --lcov --output-path tmp/lcov.info
    cargo crap --lcov tmp/lcov.info {{ crap_excludes }} \
        --format json --output tmp/crap-full.json
    WS_ROOT="$(pwd)"
    # Paths are made workspace-relative on the way in, diagnostics included.
    # cargo-crap 0.4+ reports the files it saw in LCOV but did not analyze, by
    # absolute path — committing that would put a developer's home directory in
    # the repo and churn the baseline on every machine that regenerates it.
    jq --argjson t {{ crap_threshold }} --arg ws "$WS_ROOT/" \
        '.entries |= (map(select(.crap > $t)) | map(.file |= sub("^" + $ws; "")))
         | if has("diagnostics") then
               .diagnostics.lcov_only.examples |= map(sub("^" + $ws; ""))
             | .diagnostics.source_only.examples |= map(sub("^" + $ws; ""))
           else . end' \
        tmp/crap-full.json > .cargo-crap.json
    echo "Wrote .cargo-crap.json with $(jq '.entries | length' .cargo-crap.json) over-threshold entries"

# Check the per-function coverage floor.
coverage:
    #!/usr/bin/env bash
    set -euo pipefail
    # GOTCHA: cargo-crap's `coverage` column is a PERCENTAGE (0-100), not a
    # fraction — the predicate is `.coverage < 90`, NOT `< 0.90` (which would
    # test 0.9% and pass everything).
    {{ cov_setup }}
    if [ -z "${REUSE_LCOV:-}" ]; then
        cargo llvm-cov --workspace --lcov --output-path tmp/lcov.info >/dev/null
    fi
    cargo crap --lcov tmp/lcov.info {{ crap_excludes }} --format json --output tmp/crap-now.json >/dev/null
    UNDER=$(jq --argjson f {{ cov_floor }} -r '[.entries[]|select(.coverage<$f)]|length' tmp/crap-now.json)
    if [ "$UNDER" -gt 0 ]; then
        echo "Coverage floor FAILED: $UNDER function(s) below {{ cov_floor }}%"
        jq --argjson f {{ cov_floor }} -r '.entries[]|select(.coverage<$f)|"  \(.coverage)%  \(.function)  [\(.file|sub(".*/";""))]"' tmp/crap-now.json | sort -n
        exit 1
    fi
    echo "Coverage floor PASSED: every function ≥{{ cov_floor }}% line coverage"

# List source files over 30k.
large-files:
    #!/usr/bin/env bash
    set -euo pipefail
    found=$(find crates -type f -name '*.rs' -not -path '*/target/*' -size +30k || true)
    if [ -z "$found" ]; then
        echo "(no source files over 30k)"
    else
        echo "$found" | xargs wc -lc | grep -v ' total$' | sort -rnk2 \
            | awk '{printf "  %6s lines  %7.1f KB  %s\n",$1,$2/1024,$3}'
    fi

# Install the binary to ~/.cargo/bin, and tlx at the pinned rev beside its `rev` file.
install:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo install --path crates/*/ --force
    # tlx as the image builds it (tlx-engine E8): inside its own clone, so
    # rustup takes tlx's toolchain and cargo its lockfile.
    rev=$(cat tlx.rev)
    src=tmp/tlx-src
    url=$(cat tlx.repo)
    [ -d "$src/.git" ] || git clone --quiet "$url" "$src"
    git -C "$src" fetch --quiet "$url" "$rev"
    git -C "$src" checkout --quiet "$rev"
    (cd "$src" && cargo build --release --locked -p tlx-cli)
    dest=${TLCQ_TLX:-${XDG_DATA_HOME:-$HOME/.local/share}/tlcq/tlx/tlx}
    mkdir -p "$(dirname "$dest")"
    # Replace, never overwrite: macOS keeps a signed binary's old signature for
    # a file written in place, and kills the new one at every start.
    cp "$src/target/release/tlx" "$dest.new"
    mv -f "$dest.new" "$dest"
    echo "$rev" > "$(dirname "$dest")/rev"
    echo "tlx $rev: $dest"

# Clear files from ./tmp.
tmp:
    @mkdir -p tmp
    @find tmp -maxdepth 1 -type f -delete

# Copy the laptop's TLC jars into the image's build context, so both sides hash the same jars.
docker-jars from=env("TLCQ_JARS", ""):
    #!/usr/bin/env bash
    set -euo pipefail
    [ -n "{{ from }}" ] || { echo "usage: just docker-jars <dir with tla2tools.jar> (or set TLCQ_JARS)" >&2; exit 2; }
    mkdir -p docker/jars
    cp "{{ from }}/tla2tools.jar" "{{ from }}/CommunityModules-deps.jar" docker/jars/

# Build and start the runner in local Docker (Docker Desktop's small VM: its 4
# CPUs split between the slots, no reserve).
docker-up:
    TLCQ_E_CPUS=0-1 TLCQ_P_CPUS=2-3 TLCQ_RESERVE_MB=512 TLCQ_DISK_FLOOR_GB=1 docker compose -f docker/compose.yaml up -d --build

# The runner's page (ui/) with live reload, its API proxied to a runner
# ($TLCQ_API, default https://tlcq.pve.local).
ui-dev:
    cd ui && pnpm install && pnpm dev

# Type-check the page. Nothing is built here: the image builds it.
ui-check:
    cd ui && pnpm install --frozen-lockfile && pnpm typecheck

# Build the image on pve and (re)start the runner there, behind traefik at
# tlcq.pve.local. The classic builder: podman's API has no BuildKit session,
# and buildah reads the Dockerfile's cache mounts itself.
docker-pve uid="1000":
    DOCKER_SOCKET=/run/user/{{ uid }}/podman/podman.sock DOCKER_BUILDKIT=0 docker --context pve compose -f docker/compose.yaml -f docker/compose.pve.yaml up -d --build

# Stop the local runner (the state volume stays).
docker-down:
    docker compose -f docker/compose.yaml down

# Serve one VM verb in the local runner, its payload on stdin.
docker-vm verb:
    docker exec -i tlcq-runner tlcq vm {{ verb }}

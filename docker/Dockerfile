# The tlcq image (design D9, D10): the runner, and every attempt's container,
# with both engines: tlx at the rev tlx.rev pins, and TLC.
#
# Build from the repository root, after `just docker-jars` has copied the
# laptop's TLC jars into docker/jars/ (the VM recomputes job ids with its own
# jars and tlx rev, so both sides must hold the same ones):
#
#     docker compose -f docker/compose.yaml build

FROM rust:1.99 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin tlcq && cp target/release/tlcq /usr/local/bin/tlcq

# tlx (tlx-engine E8) at the rev tlx.rev pins, from the repository tlx.repo
# names, built inside its clone so that rustup takes tlx's own toolchain and
# cargo its lockfile, as `just install` does on the laptop. A layer of its own:
# a rebuild for a tlcq change keeps the same binary, whose checkpoints a restart
# can then recover.
FROM rust:1.99 AS tlx
COPY tlx.rev tlx.repo /
RUN git clone --quiet "$(cat /tlx.repo)" /src/tlx \
    && git -C /src/tlx fetch --quiet origin "$(cat /tlx.rev)" \
    && git -C /src/tlx checkout --quiet "$(cat /tlx.rev)"
WORKDIR /src/tlx
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/tlx/target \
    cargo build --release --locked -p tlx-cli && cp target/release/tlx /usr/local/bin/tlx

# The runner's web page (D16): the React app in ui/, served from TLCQ_UI_DIR.
FROM node:22-slim AS ui
WORKDIR /ui
RUN corepack enable
COPY ui/package.json ui/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY ui ./
RUN pnpm build

# Only the CLI: the runner starts attempts through the Docker (or podman) socket
# mounted into it.
FROM docker:28-cli AS docker-cli

FROM eclipse-temurin:25-jre
# tlx builds its native code with a C compiler at run time (else it keeps its
# JIT code: slower, the same results).
RUN apt-get update \
    && apt-get install -y --no-install-recommends gcc libc6-dev \
    && rm -rf /var/lib/apt/lists/*
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=build /usr/local/bin/tlcq /usr/local/bin/tlcq
COPY --from=tlx /usr/local/bin/tlx /opt/tlcq/tlx/tlx
COPY tlx.rev /opt/tlcq/tlx/rev
COPY docker/jars/tla2tools.jar docker/jars/CommunityModules-deps.jar /opt/tlcq/jars/
COPY --from=ui /ui/dist /opt/tlcq/ui
# tlx's native-code cache is on the state volume, which every attempt mounts,
# so a configuration run again loads its build at once.
ENV TLCQ_STATE=/state \
    TLCQ_JARS=/opt/tlcq/jars \
    TLCQ_UI_DIR=/opt/tlcq/ui \
    TLCQ_JAVA=java \
    TLCQ_TLX=/opt/tlcq/tlx/tlx \
    TLX_CACHE=/state/tlx-cache \
    CC=gcc
VOLUME /state
CMD ["tlcq", "runner"]

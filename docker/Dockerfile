# syntax=docker/dockerfile:1.7
#
# vox_room in a container: the Svelte SPA, then the Rust binary, then a
# slim runtime that fetches its speech-to-text models on first start.
#
#   docker compose build
#
# Nothing here needs PipeWire or a sound card: all the audio is the
# browsers'. See ROOM.md.

ARG NODE_IMAGE=node:24-bookworm-slim
ARG RUST_IMAGE=rust:1-bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim
ARG PNPM_VERSION=11.27.0

# --- The web app --------------------------------------------------------
# build.rs normally runs this; here it is its own stage so the Rust build
# doesn't need node, and `pnpm install` is cached on the lockfile alone.
FROM ${NODE_IMAGE} AS ui
ARG PNPM_VERSION
RUN npm install -g pnpm@${PNPM_VERSION}
WORKDIR /web
COPY crates/vox_room/web/package.json \
     crates/vox_room/web/pnpm-lock.yaml \
     crates/vox_room/web/pnpm-workspace.yaml ./
RUN --mount=type=cache,target=/pnpm/store,sharing=locked \
    pnpm config set store-dir /pnpm/store && pnpm install --frozen-lockfile
COPY crates/vox_room/web/ ./
# vite.config.js puts the bundle in ../dist-ui, so /dist-ui.
RUN pnpm run build

# --- The binary ---------------------------------------------------------
FROM ${RUST_IMAGE} AS build
# libopus for the room's 20 ms frames; sherpa-onnx-sys downloads its own
# prebuilt static libs, and rusqlite bundles SQLite.
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libopus-dev \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
COPY --from=ui /dist-ui ./crates/vox_room/dist-ui
# rust-embed bakes dist-ui into the binary; the UI is already built.
ENV VOX_ROOM_SKIP_UI_BUILD=1
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked -p vox_room \
 && mkdir -p /out && cp target/release/vox_room /out/vox_room

# --- Runtime ------------------------------------------------------------
FROM ${RUNTIME_IMAGE}
# libstdc++6 for sherpa-onnx's static libs, libopus0 for the codec,
# curl + bzip2 for the model download in the entrypoint.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl bzip2 libopus0 libstdc++6 \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --gid 10001 vox \
 && useradd --uid 10001 --gid vox --no-create-home --shell /usr/sbin/nologin vox \
 && mkdir -p /data /models \
 && chown vox:vox /data /models
COPY --from=build /out/vox_room /usr/local/bin/vox_room
COPY docker/entrypoint.sh /usr/local/bin/vox-room-entrypoint
# Docker gives a fresh named volume the ownership of the image's directory,
# so /data and /models come up writable for uid 10001.
ENV VOX_ROOM_LISTEN=0.0.0.0:7340 \
    VOX_ROOM_DATA=/data \
    VOX_SCRIBE_MODELS=/models
EXPOSE 7340
USER vox
ENTRYPOINT ["/usr/local/bin/vox-room-entrypoint"]

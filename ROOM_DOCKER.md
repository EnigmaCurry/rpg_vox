# vox_room in Docker

`docker/` is a self-contained deployment of the voice room (ROOM.md)
behind Traefik: a Dockerfile, a compose file whose Traefik labels are
interpolated from `docker/.env`, and an entrypoint that fetches the
speech-to-text models on first start.

It assumes nothing about where the Docker daemon is. Everything is a
named volume, so a remote context works the same as a local one.

```bash
cd docker
just config                   # asks about each setting, writes .env
just install                  # builds the image and starts the room
just logs -f                  # the first start downloads the models
just room-invite Ryan
```

| recipe | what it does |
| --- | --- |
| `config` | walks `.env.example`, asks about every key, writes `.env`. Re-runnable: your current answers become the defaults |
| `install` | build and start, or nothing if it is already up |
| `reinstall` | throw the containers away and build them again, **keeping** the volumes |
| `destroy` | containers *and* volumes, after you type `yes` |
| `start` / `stop` | stop and start without recreating anything |
| `status` | what exists, what is running, which volumes are there |
| `logs` | `just logs -f` to follow |
| `shell` | a bash shell in the container; `just shell -u root` for root |
| `room-invite` / `room-uninvite` / `room-people` | the room's CLI inside the container |
| `show-config` | the fully interpolated compose file, Traefik labels and all |

`just room-invite NAME` prints the link to send someone, which works
once: their browser trades it for a cookie when they press **Join
Audio**, and `just room-uninvite NAME` takes both away. All three CLI
recipes read and write the same SQLite file the running room has open,
which is fine — they are brief writes.

`docker/.env` is deliberately *not* the repository's own `.env` (that one
belongs to `just` and rpg_vox, and holds unrelated secrets). The compose
file reads only `docker/.env`, and uses it for both the label
interpolation and the container's environment.

## What the image is

Three stages, ending at 189 MB:

1. **node** builds the Svelte SPA. `build.rs` normally does this, but as
   its own stage the Rust build needs no node and `pnpm install` caches
   on the lockfile alone. `VOX_ROOM_SKIP_UI_BUILD=1` tells `build.rs` to
   leave the prebuilt `dist-ui/` alone, and `rust-embed` bakes it in.
2. **rust** builds `vox_room` only — no PipeWire, no sound card, because
   all the room's audio is the browsers'. It needs `libopus-dev`;
   sherpa-onnx downloads its own prebuilt static libraries and rusqlite
   bundles SQLite.
3. **debian-slim** carries the binary plus `libstdc++6` (sherpa-onnx's
   static libs link it), `libopus0`, and `curl`/`bzip2` for the models.

It runs as uid 10001 on a read-only root filesystem with every capability
dropped and `no-new-privileges`. Only `/data`, `/models` and a 64 MB
`/tmp` are writable. Docker hands a fresh named volume the ownership of
the image's directory, so both volumes come up writable without any
chown dance.

## The models

The room needs a sherpa-onnx recognizer, and `scribe` is what normally
downloads one. In the container `docker/entrypoint.sh` does it instead,
into the `models` volume, using the same archive names and directory
layout as `crates/vox_scribe/src/models.rs`:

| `VOX_ROOM_STT_MODEL` | what you get |
| --- | --- |
| `parakeet` (default) | Parakeet TDT 0.6B v2, English, ~660 MB, the most accurate |
| `sensevoice` | SenseVoice Small, multilingual, ~230 MB |
| `none` | no transcription, and so no AI — passes `--no-stt` |

Either one comes with the 20M streaming Zipformer, which is what puts
words on screen while someone is still talking. `VOX_ROOM_TTS=kokoro`
adds the Kokoro voice model the same way.

Expect the first `up` to sit there for a few minutes. The healthcheck has
a 15-minute `start_period` to cover it. After that the volume has them
and startup is seconds; the entrypoint checks for the files and skips
anything already present, so rebuilding the image does not re-download.

## Traefik

The labels are plain `${VAR}` interpolation from `docker/.env`, so
`docker compose config` shows you exactly what Traefik will read:

```
traefik.http.routers.vox-room.rule: Host(`room.example.com`)
traefik.http.routers.vox-room.entrypoints: rpgvox
traefik.http.routers.vox-room.tls: true
traefik.http.services.vox-room.loadbalancer.server.port: 7340
traefik.http.routers.vox-room.middlewares: vox-room-ipallowlist,vox-room-headers,vox-room-ratelimit
```

`7340` is the container's own port and never changes;
`VOX_ROOM_TRAEFIK_ENTRYPOINT` and `VOX_ROOM_PUBLIC_PORT` are the
public side.

Things worth knowing:

- **TLS is not optional.** Browsers only allow the microphone on HTTPS
  (or localhost), so the room is useless on a plain HTTP router. There is
  no `certresolver`: the certificate for the host is already in Traefik's
  store, and `tls=true` is enough for it to be matched by SNI.
- **The room must be at the domain root.** The SPA fetches absolute
  `/api/door` and `wss://{host}/room.ws`, and the session cookie is
  `Path=/`. A `StripPrefix` mount at `/room/` breaks all three. Give it
  its own host.
- **The public port belongs in the invite links.** Invites are built
  from `VOX_ROOM_URL`, so an entrypoint that isn't on 443 — the `rpgvox`
  entrypoint on 7331, say — needs `VOX_ROOM_URL=https://host:7331` or
  every link you hand out goes nowhere. `just config` composes it from
  `VOX_ROOM_TRAEFIK_HOST` and `VOX_ROOM_PUBLIC_PORT`, and compose refuses
  to start without it rather than serving silently wrong links.
- **`X-Forwarded-Proto` is load-bearing.** The room marks its session
  cookie `Secure` only when it sees `X-Forwarded-Proto: https`. Traefik
  sends it; anything you put in front of Traefik must preserve it.
- **Stops are SIGINT, not SIGTERM.** `vox_room` waits on `ctrl_c`, and
  as PID 1 it ignores SIGTERM, so without `stop_signal: SIGINT` every
  stop sat out the grace period and ended in SIGKILL (10s, exit 137).
  With it, a stop is immediate and clean.
- **WebSockets need no configuration** — Traefik upgrades `/room.ws` on
  the same port natively. Don't add a buffering middleware; it would
  break the audio stream. An idle socket (a tab that hasn't pressed
  **Join Audio**) can hit `respondingTimeouts.idleTimeout`, but the
  client reconnects with backoff, so it self-heals.
- **`VOX_ROOM_IP_SOURCERANGE`** gates who may reach the room at all,
  before the invite cookie is even looked at. `0.0.0.0/0` is everyone;
  narrowing it makes the invite links a second lock rather than the only
  one.
- **The rate limit is generous on purpose** (20/s per IP, burst 60). The
  room is a few HTTP requests plus one long-lived WebSocket per tab, so
  it never trips, but it stops anyone hammering `/join/`.
- **Keep the invite token out of your access log.** Invites are
  `/join/<token>`, and the token *is* the credential until it is spent.
  It works once — whoever presses **Join Audio** with it gets the cookie
  and nobody else can — but a link sitting unused in a log is a way in.
  If you log request paths, redact or drop `/join/`.

There is no `networks:` block and no `traefik.docker.network` label:
Traefik is assumed to run on the host network, so it reaches the
container on the compose project's own bridge network at the address the
Docker provider reads off the container. Nothing to create, nothing to
attach to, nothing to keep in sync.

## Reaching your model servers

`host.docker.internal` is mapped (`extra_hosts: host-gateway`), but it
means the machine the *daemon* runs on. With a remote context that is the
remote host, not your workstation, so `VOX_SCRIBE_LLM_URL` and
`VOX_ROOM_TTS_URL` need names the remote host can resolve.

## Blank is not unset

`docker compose` passes `FOO=` through as an empty variable, and
`vox_room` takes it literally. An empty `VOX_ROOM_PROMPT` is an empty
system prompt; an empty `VOX_ROOM_TTS_URL` is a Qwen3-TTS server at no
address; an empty `VOX_ROOM_TTS` fails to parse and the room won't start.
Comment the line out instead of emptying it. `.env.example` has the
hazardous ones commented rather than blank.

# rpg_vox

![rpg_vox](rpg_vox.png)

rpg_vox is an audio bridge and chat bot for RPGs played over a video
conference. It speaks lines for your characters with text-to-speech,
transcribes the table as you play, and routes all of it through
PipeWire. The first target is Discord, but because the core is plain
PipeWire nodes, it can be patched into any voice or streaming setup.
rpg_vox runs on Linux.

For transcription on its own, without the RPG tooling and on macOS too,
see the companion app [scribe](SCRIBE.md).

## What's in the workspace

| Crate | What it is |
|---|---|
| `crates/rpg_vox` | The main app: a Rust backend with an embedded Svelte web UI, served on `http://127.0.0.1:7331`. Registers a PipeWire source (a virtual mic) for its speech and PipeWire sinks ("vox channels") for the audio it transcribes. |
| `crates/discord_vox` | A Discord voice bot. Registers a PipeWire sink whose audio becomes the bot's mic in a voice channel, and can record each speaker and send speaker hints to rpg_vox. |
| `crates/vox_transcribe` | Layered live speech-to-text, shared by rpg_vox's Record page and scribe. |
| `crates/vox_audio` | Audio capture for scribe (PipeWire, CoreAudio, files) and Opus recording. |
| `crates/vox_scribe` | [scribe](SCRIBE.md), the standalone transcription TUI. |

## Features

* **Speech synthesis to a virtual mic.** Text sent to rpg_vox is
  rendered by a TTS backend and played into its PipeWire source node
  (`rpg-vox`), which shows up as a microphone in Firefox, Discord, OBS or
  anything else. Backends are remote:
  * `qwen3` (default): a Qwen3-TTS Gradio deployment, with preset
    speakers, voice cloning from a short reference clip, or voices
    designed from a text description.
  * `comfyui`: a ComfyUI instance running any workflow in `workflows/`
    that contains `{{TEXT}}` where the line goes.
* **Characters and pronunciation.** Projects hold characters with their
  own voice profiles, and a dictionary of phonetic respellings applied
  just before synthesis, so stubborn proper nouns come out right.
* **Script and scenes.** The Script page is an LLM chat (any
  OpenAI-compatible endpoint) whose replies mark spoken lines with
  `<speak>…</speak>`; each one becomes an inline player you can render
  takes of and send to the mic. The Scenes page lays prepared lines out in
  lanes for playback during a game.
* **Live transcription.** Each vox channel is a PipeWire sink: route the
  Discord call (or any app) into it and the Record page transcribes it in
  layers, the same as scribe: instant streaming text in ALL CAPS, then a
  higher-quality re-decode of each utterance, then a pass that fixes the
  words at utterance boundaries. With discord_vox's speaker hints, lines
  are attributed to the Discord user who said them. Recordings are kept
  with their audio.
* **Mixer.** A strip per vox channel with levels, and a view of the
  PipeWire graph.
* **Web mic and overlays.** Browsers can stream a mic into rpg_vox over a
  WebSocket, and `/obs/subtitles` is a live subtitle overlay for OBS.
* **HTTP API.** `POST /say` speaks text from scripts and other tools
  (`just say "hello world"`).

## Running it

All recipes run inside the project's `nix-shell`, which provides Rust,
the PipeWire headers and the rest of the toolchain.

```bash
just dev                  # backend + Vite UI with hot reload: http://127.0.0.1:5173
just run                  # release build: http://127.0.0.1:7331
just run --comfyui http://192.168.1.10:8188 --tts-backend comfyui
just say "hello world"    # speak through a running instance
just list-sources         # check the rpg-vox node is in the PipeWire graph
```

`just dev` downloads the speech-to-text models into `models/` on first
run (`just download-stt-model`, `just download-streaming-stt-model`).
Data (a SQLite database and rendered clips) lives in `data/`
(`--data-dir`). `just run -- --help` lists every option; most also read
an environment variable, and `.env` in the repo root is loaded by every
recipe. The main ones:

| Variable | Option | Purpose |
|---|---|---|
| `RPG_VOX_TTS_BACKEND` | `--tts-backend` | `qwen3` (default) or `comfyui`. |
| `RPG_VOX_QWEN3_PRESETS_URL` | `--qwen3-presets-url` | Qwen3-TTS deployment for preset speakers (required for `qwen3`). |
| `RPG_VOX_QWEN3_CLONE_URL`, `RPG_VOX_QWEN3_DESIGN_URL` | | Optional deployments for cloned and designed voices. |
| `RPG_VOX_COMFYUI` | `--comfyui` | ComfyUI base URL, default `http://127.0.0.1:8188`. |
| `RPG_VOX_CHAT_BASE_URL`, `RPG_VOX_CHAT_MODEL`, `RPG_VOX_CHAT_API_KEY` | `--chat-*` | The OpenAI-compatible LLM for the Script page. `just llama_rocm` serves one locally with llama.cpp. |
| `RPG_VOX_NAME` | `--node-name` | PipeWire node name, to run several instances side by side. |
| `RPG_VOX_AUTO_LINK` | `--auto-link` | A sink to link the rpg-vox source to on startup, e.g. `discord-vox`. |
| `RPG_VOX_DATA_DIR` | `--data-dir` | Where projects, clips and recordings are stored. |

The listen address is `--bind` (default `127.0.0.1:7331`) and the number
of vox channels is `--vox-slots`.

## Discord

discord_vox is a separate bot process. It joins a configured voice
channel when a person is in it and leaves when it empties; whatever is
routed into its `discord-vox` sink is the bot's voice there.

```bash
just discord              # needs DISCORD_VOX_TOKEN, DISCORD_VOX_GUILD_ID,
                          # DISCORD_VOX_CHANNEL_ID (in .env or as flags)
just list-sinks           # check the discord-vox node is in the graph
```

Link rpg_vox to it with `--auto-link discord-vox` (or Helvum / qpwgraph /
`pw-link`). Options worth knowing:

* `--record DIR` (`DISCORD_VOX_RECORD_DIR`): record each session as one
  WAV per speaker plus a mix. Recording needs consent; the bot posts a
  notice naming the operator (`DISCORD_VOX_OPERATOR_NAME`,
  `DISCORD_VOX_OPERATOR_DISCORD`, `DISCORD_VOX_OPERATOR_EMAIL`) in the
  voice channel's text chat.
* `DISCORD_VOX_AUDIO_OUTPUT_1`, `_2`, …: Discord user ids to give their
  own PipeWire source node, so each player can be transcribed separately.
* `--rpg-vox-url URL` (`DISCORD_VOX_RPG_VOX_URL`): send speaking hints to
  rpg_vox so the Record page knows who said each line.

## Development

```bash
just check     # cargo check the workspace
just clippy    # lints, warnings are errors
just test      # tests
just fmt       # format
just dev-ui    # only the Vite UI, proxying to a running backend
```

rpg_vox and discord_vox need PipeWire and build on Linux only; on macOS
only scribe builds (see [SCRIBE.md](SCRIBE.md#install)).

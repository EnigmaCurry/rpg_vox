# rpg_vox workspace task runner. All recipes run inside the project's nix-shell.

# Load .env from the workspace root into every recipe's environment (e.g.
# DISCORD_TOKEN / DISCORD_GUILD_ID / DISCORD_CHANNEL_ID for discord_vox).
set dotenv-load

# Default: show recipes.
default:
    @just --list

# Interactive dev shell with rustc, cargo, pipewire headers, etc.
shell:
    nix-shell

# Debug build (whole workspace).
build:
    nix-shell --run "cargo build"

# Optimized build (whole workspace).
release:
    nix-shell --run "cargo build --release"

# Fast type-check without codegen (whole workspace).
check:
    nix-shell --run "cargo check --workspace --all-targets"

# Lints, fail on warnings.
clippy:
    nix-shell --run "cargo clippy --workspace --all-targets -- -D warnings"

# Format.
fmt:
    nix-shell --run "cargo fmt --all"

# Tests.
test:
    nix-shell --run "cargo test --workspace"

# --- rpg_vox (TTS → virtual mic) ---

# Run the rpg_vox release binary. Extra args are forwarded, e.g.
#   just run -- --comfyui http://192.168.1.10:8188
run *ARGS:
    nix-shell --run "cargo run -p rpg_vox --release -- {{ARGS}}"

# Auto-rebuild + restart on any change under the rpg_vox crate. Watches Rust
# sources only — for hot-reloading the Svelte UI use `just dev-ui` alongside
# this in a second terminal (Vite serves at :5173 with /say etc. proxied to
# the running backend). `download-stt-model` is a dep so a fresh checkout
# gets the STT model on first `just dev` — the recipe itself no-ops when
# the files are already there, so the wait only happens once.
dev *ARGS: download-stt-model
    nix-shell --run "cargo watch -q -c -w crates/rpg_vox/src -w crates/rpg_vox/Cargo.toml -x 'run -p rpg_vox -- {{ARGS}}'"

# Serve the Svelte SPA via Vite on http://127.0.0.1:5173 with hot reload;
# API requests (/say, /chat, /settings, /pw/*, /healthz, /workflows,
# /workflow/*) are proxied to the running rpg_vox backend on :7331.
# Override the backend with RPG_VOX_BACKEND=http://host:port just dev-ui.
dev-ui:
    nix-shell --run "cd crates/rpg_vox/web && pnpm install && pnpm run dev"

# Build the SPA into crates/rpg_vox/dist-ui/ (embedded by the release binary).
# Usually not needed manually — `just build` / `just run` invoke build.rs which
# runs this automatically. Handy for CI / packaging where you want to prebuild
# and pass RPG_VOX_SKIP_UI_BUILD=1 to cargo.
build-ui:
    nix-shell --run "cd crates/rpg_vox/web && pnpm install && pnpm run build"

# Run rpg_vox against a remote Qwen3-TTS Gradio deployment. Configure the
# endpoint via --qwen3-url (or RPG_VOX_QWEN3_URL); the default points at the
# user's hosted instance. Extra ARGS append to clap, e.g.
#   just qwen3 --qwen3-speaker Aiden --qwen3-language English
qwen3 *ARGS:
    nix-shell --run "cargo run -p rpg_vox --release -- --tts-backend qwen3 {{ARGS}}"

# Post text to a running rpg_vox instance.
#   just say "hello world"
say TEXT bind="127.0.0.1:7331":
    curl -sS -X POST http://{{bind}}/say \
        -H 'content-type: application/json' \
        --data-binary @<(jq -Rn --arg t "{{TEXT}}" '{text:$t}')
    @echo

# Verify the rpg_vox source node is visible to PipeWire.
list-sources:
    pw-cli list-objects Node | grep -A1 -B1 "rpg-vox" || echo "rpg-vox not found — is the bridge running?"

# Download the SenseVoice int8 STT model into models/sense-voice/. Used by
# /widgets/record to transcribe captured audio into the SpeakCell caption.
# ~230 MB one-time download from the k2-fsa GitHub release; skipped if
# the target files already exist.
download-stt-model:
    #!/usr/bin/env bash
    set -euo pipefail
    dest="models/sense-voice"
    if [ -f "$dest/model.int8.onnx" ] && [ -f "$dest/tokens.txt" ]; then
      echo "sense-voice model already present at $dest/"
      exit 0
    fi
    mkdir -p models
    stem="sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17"
    archive="$stem.tar.bz2"
    url="https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/$archive"
    tmp="$(mktemp -d)"
    trap "rm -rf '$tmp'" EXIT
    echo "downloading $archive …"
    curl -fL --progress-bar -o "$tmp/$archive" "$url"
    tar -xjf "$tmp/$archive" -C "$tmp"
    mkdir -p "$dest"
    cp "$tmp/$stem/model.int8.onnx" "$dest/"
    cp "$tmp/$stem/tokens.txt" "$dest/"
    echo "installed sense-voice model → $dest/"

# --- discord_vox (PipeWire sink → Discord voice) ---

# Run the discord_vox release binary. Expects DISCORD_TOKEN / DISCORD_GUILD_ID /
# DISCORD_CHANNEL_ID in the env (or pass --token / --guild-id / --channel-id).
discord *ARGS:
    nix-shell --run "cargo run -p discord_vox --release -- {{ARGS}}"

# Auto-rebuild + restart on changes to the discord_vox crate.
discord-dev *ARGS:
    nix-shell --run "cargo watch -q -c -w crates/discord_vox/src -w crates/discord_vox/Cargo.toml -x 'run -p discord_vox -- {{ARGS}}'"

# Verify the discord_vox sink is visible to PipeWire.
list-sinks:
    pw-cli list-objects Node | grep -A1 -B1 "discord-vox" || echo "discord-vox not found — is the bridge running?"

# Clean build artifacts.
clean:
    cargo clean

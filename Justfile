# rpg_vox workspace task runner. All recipes run inside the project's nix-shell.

# Load .env from the workspace root into every recipe's environment (e.g.
# DISCORD_VOX_TOKEN / DISCORD_VOX_GUILD_ID / DISCORD_VOX_CHANNEL_ID for discord_vox).
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

# Release archives in dist/: one tarball per binary, named
# <bin>-<VERSION>-<target>.tar.gz, plus a .sha256 beside each. scribe
# builds everywhere; rpg_vox needs PipeWire, so it ships on Linux only.
# Uses nix-shell when available, plain cargo otherwise (macOS, CI).
# The release workflow runs this on each target.
dist VERSION=`git describe --tags --always --dirty`:
    #!/usr/bin/env bash
    set -euo pipefail
    bins=(scribe agent)
    pkgs=(-p vox_scribe -p vox_agent)
    if [ "$(uname)" = Linux ]; then
      bins+=(rpg_vox)
      pkgs+=(-p rpg_vox)
    fi
    build="cargo build --release --locked ${pkgs[*]}"
    if command -v nix-shell >/dev/null; then
      nix-shell --run "$build"
    else
      $build
    fi
    target="$(rustc -vV | sed -n 's/^host: //p')"
    exe=""
    case "$target" in *-windows-*) exe=".exe" ;; esac
    mkdir -p dist
    for bin in "${bins[@]}"; do
      name="$bin-{{VERSION}}-$target"
      stage="dist/$name"
      rm -rf "$stage"
      mkdir -p "$stage"
      cp "target/release/$bin$exe" LICENSE.txt README.md "$stage/"
      case "$bin" in
        scribe) cp SCRIBE.md SCRIBE_REFERENCE.md "$stage/" ;;
        agent) cp AGENT.md "$stage/" ;;
        rpg_vox) cp RPG_VOX.md "$stage/" ;;
      esac
      if [ -n "$exe" ]; then
        # The C runtime the binaries link against (see
        # .cargo/config.toml) is in a Microsoft redistributable rather
        # than in Windows itself, so a copy goes beside them: a
        # program's own directory is the first place Windows looks for
        # a DLL, so the unpacked folder needs nothing installed.
        sys32="$(cygpath -S 2>/dev/null || true)"
        : "${sys32:=/c/Windows/System32}"
        for dll in vcruntime140.dll vcruntime140_1.dll; do
          if [ -f "$sys32/$dll" ]; then
            cp "$sys32/$dll" "$stage/"
          else
            echo "warning: no $sys32/$dll to ship alongside $bin$exe;" \
                 "unpacking this archive will need the Visual C++" \
                 "redistributable installed" >&2
          fi
        done
        # A .zip, which Explorer opens on its own. Git's bash has no
        # `zip`, and its `tar` is GNU tar, which writes no zip either;
        # Windows' own tar is libarchive's, and `-a` takes the format
        # from the name. PowerShell's Compress-Archive would do it in
        # one call, but writes `dir\file` with a backslash, which
        # Explorer reads and `unzip` anywhere else takes for part of
        # the filename.
        archive="$name.zip"
        rm -f "dist/$archive"
        if [ ! -x "$sys32/tar.exe" ]; then
          echo "no $sys32/tar.exe to make the .zip with" >&2
          exit 1
        fi
        (cd dist && "$sys32/tar.exe" -a -cf "$archive" "$name")
      else
        archive="$name.tar.gz"
        tar -czf "dist/$archive" -C dist "$name"
      fi
      rm -rf "$stage"
      sum=shasum
      command -v shasum >/dev/null || sum=sha256sum
      case "$sum" in
        shasum) (cd dist && shasum -a 256 "$archive" > "$archive.sha256") ;;
        *) (cd dist && sha256sum "$archive" > "$archive.sha256") ;;
      esac
      echo "packaged dist/$archive"
    done

# Cut a release from master: bump the workspace version (prompts, default
# next patch), name CHANGELOG.md's "## Unreleased" section after it (and
# open a new one), commit, tag v<version> and push both after
# confirmation. The tag push runs the release workflow. Needs a clean,
# up-to-date master (untracked files are ignored) and CHANGELOG.md entries
# under "## Unreleased", committed since the last release.
publish:
    #!/usr/bin/env bash
    set -euo pipefail
    branch="$(git rev-parse --abbrev-ref HEAD)"
    [ "$branch" = master ] || { echo "publish from master, not $branch" >&2; exit 1; }
    [ -z "$(git status --porcelain --untracked-files=no)" ] \
      || { echo "uncommitted changes:" >&2; git status --short --untracked-files=no >&2; exit 1; }
    git fetch --quiet origin master --tags
    [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/master)" ] \
      || { echo "master differs from origin/master; pull or push first" >&2; exit 1; }
    last="$(git describe --tags --abbrev=0 2>/dev/null || true)"
    if [ -n "$last" ] && git diff --quiet "$last" HEAD -- CHANGELOG.md; then
      echo "CHANGELOG.md hasn't changed since $last; add this release's changes under ## Unreleased" >&2
      exit 1
    fi
    unreleased="$(awk '/^## /{on = ($0 == "## Unreleased"); next} on && NF' CHANGELOG.md)"
    [ -n "$unreleased" ] \
      || { echo "CHANGELOG.md has nothing under ## Unreleased" >&2; exit 1; }
    current="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)"
    IFS=. read -r major minor patch <<< "${current%%-*}"
    default="$major.$minor.$((patch + 1))"
    read -rp "Current version $current. New version [$default]: " version
    version="${version:-$default}"
    [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] \
      || { echo "not a semver version: $version" >&2; exit 1; }
    tag="v$version"
    ! git rev-parse -q --verify "refs/tags/$tag" >/dev/null \
      || { echo "tag $tag already exists" >&2; exit 1; }
    sed -i.bak '/^\[workspace.package\]/,/^\[/s/^version = ".*"/version = "'"$version"'"/' Cargo.toml
    rm Cargo.toml.bak
    # "## Unreleased" becomes this version, under a fresh empty one.
    awk -v head="## $tag ($(date +%Y-%m-%d))" \
      '$0 == "## Unreleased" && !done {print; print ""; print head; done = 1; next} {print}' \
      CHANGELOG.md > CHANGELOG.md.new && mv CHANGELOG.md.new CHANGELOG.md
    cargo update --workspace --quiet
    git --no-pager diff --stat
    read -rp "Commit, tag $tag and push to origin? [y/N] " ok
    if [ "$ok" != y ] && [ "$ok" != Y ]; then
      git checkout -- Cargo.toml Cargo.lock CHANGELOG.md
      echo "aborted; version change reverted"
      exit 1
    fi
    git commit -q -m "Release $tag" Cargo.toml Cargo.lock CHANGELOG.md
    git tag -a "$tag" -m "Release $tag"
    git push --atomic origin master "$tag"
    echo "pushed $tag; the release workflow is building it"

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

# Auto-rebuild + restart on any change under the rpg_vox crate, AND serve the
# Svelte SPA via Vite with hot-reload — both in one terminal. Point your
# browser at http://127.0.0.1:5173 for the dev UI: Vite HMR picks up .svelte
# edits instantly (no rebuild), and API requests (/say, /chat, /monitor.ws,
# etc.) are proxied to the rpg_vox backend on :7331 which cargo-watch rebuilds
# on Rust changes. Ctrl-C tears down both children. Both model recipes are
# deps so a fresh checkout gets the STT + streaming-STT models on first `just
# dev`; each recipe no-ops when the files are already there, so the wait only
# happens once. If you'd rather run the pieces separately, `just dev-ui` and
# a manual `cargo run` still work.
dev *ARGS: download-stt-model download-streaming-stt-model
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill $(jobs -p) 2>/dev/null || true' EXIT INT TERM
    printf '\n\033[1;32m▶ rpg_vox dev\033[0m — open \033[1;36mhttp://127.0.0.1:5173\033[0m in your browser\n'
    printf '   • :5173 → Vite dev server. Svelte edits hot-reload here. API is proxied to :7331.\n'
    printf '   • :7331 → Rust backend + embedded prod SPA. Only refreshes on Rust rebuild + reload.\n\n'
    # `cargo watch` without `-c` so the banner + Vite'\''s "ready" line stay
    # visible instead of getting cleared on every Rust rebuild.
    nix-shell --run "cargo watch -q -w crates/rpg_vox/src -w crates/rpg_vox/Cargo.toml -x 'run -p rpg_vox -- {{ARGS}}'" &
    nix-shell --run "cd crates/rpg_vox/web && pnpm install && pnpm run dev" &
    wait -n

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

# --- LLM backend (llama.cpp — feeds /script via OpenAI-compat /v1) ---

# Run llama.cpp's llama-server locally for the /script chat page. Pulls the
# GGUF from Hugging Face on first launch and serves the OpenAI-compatible
# API on --host/--port. Defaults match rpg_vox's chat client env
# (RPG_VOX_CHAT_BASE_URL=http://127.0.0.1:9931/v1 + Muse-Glimmer-30B).
#
# Overridable via env (put in .env or export in the shell):
#   LLAMA_HF_REPO     — Hugging Face repo id
#   LLAMA_HF_FILE     — file inside the repo (.gguf)
#   LLAMA_HOST        — bind host (default 127.0.0.1)
#   LLAMA_PORT        — bind port (default 9931)
#   LLAMA_GPU_LAYERS  — -ngl value (default "all"; use "0" for CPU-only)
#   CONTEXT_SIZE      — -c value (default 131072 — tested working on 24GB VRAM)
#
# Pins the llama.cpp flake to a specific commit (stored in .llama-cpp.rev)
# so the ROCm build stays in the /nix/store between runs — otherwise each
# `nix shell github:ggml-org/llama.cpp#rocm` follows upstream HEAD and
# recompiles whenever nixpkgs or llama.cpp move. Bump the pin with
# `just llama_rocm_update`.
#
# --reasoning-format=none + --no-reasoning-preserve keep `<think>` blocks
# inside `content` so rpg_vox's own strip pass handles them. Extra flags
# are forwarded, e.g. `just llama_rocm --n-cpu-moe 24`.
llama_rocm *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    hf_repo="${LLAMA_HF_REPO:-meta-models/Muse-Glimmer-30B-GGUF}"
    hf_file="${LLAMA_HF_FILE:-Muse-Glimmer-30B-KQuant-17GB-Q4_K_M.gguf}"
    port="${LLAMA_PORT:-9931}"
    host="${LLAMA_HOST:-127.0.0.1}"
    gpu_layers="${LLAMA_GPU_LAYERS:-all}"
    context_size="${CONTEXT_SIZE:-131072}"
    lockfile=".llama-cpp.rev"
    if [[ ! -s "$lockfile" ]]; then
        echo "resolving latest github:ggml-org/llama.cpp commit (one-time)…"
        rev=$(nix flake metadata --refresh --no-write-lock-file --json github:ggml-org/llama.cpp | jq -r .locked.rev)
        if [[ -z "$rev" || "$rev" == "null" ]]; then
            echo "failed to resolve llama.cpp revision" >&2
            exit 1
        fi
        echo "$rev" > "$lockfile"
        echo "pinned llama.cpp @ $rev → $lockfile (bump with: just llama_rocm_update)"
    fi
    rev=$(cat "$lockfile")
    echo "llama.cpp (rocm) @ ${rev:0:12}: model=$hf_repo/$hf_file listening on $host:$port (ctx=$context_size)"
    nix shell --no-write-lock-file "github:ggml-org/llama.cpp/$rev#rocm" -c llama-server \
        -hf "$hf_repo" \
        -hff "$hf_file" \
        --no-mmproj \
        -ngl "$gpu_layers" \
        -c "$context_size" \
        -np 1 \
        -fa on \
        --host "$host" \
        --port "$port" \
        --reasoning-format none \
        --no-reasoning-preserve \
        {{ARGS}}

# Refresh the .llama-cpp.rev pin to the current github:ggml-org/llama.cpp
# HEAD. Next `just llama_rocm` will fetch + recompile against the new rev.
llama_rocm_update:
    #!/usr/bin/env bash
    set -euo pipefail
    rev=$(nix flake metadata --refresh --no-write-lock-file --json github:ggml-org/llama.cpp | jq -r .locked.rev)
    if [[ -z "$rev" || "$rev" == "null" ]]; then
        echo "failed to resolve llama.cpp revision" >&2
        exit 1
    fi
    echo "$rev" > .llama-cpp.rev
    echo "pinned llama.cpp @ $rev"

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
    stem="sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17"
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

# Download the streaming Zipformer transducer into models/streaming-zipformer/.
# Powers the Record page's live word-by-word transcript so text streams as
# it's spoken (see crates/rpg_vox/src/stt.rs :: StreamingSttHandle). Uses
# the small English 20M int8 bundle (~40 MB) which is CPU-friendly enough
# for real-time on a laptop; swap for a larger bundle by editing this
# recipe or by pointing --streaming-stt-* CLI flags at your own files.
# Files are renamed on install to match the CLI defaults so no extra
# flags are needed.
download-streaming-stt-model:
    #!/usr/bin/env bash
    set -euo pipefail
    dest="models/streaming-zipformer"
    if [ -f "$dest/encoder.onnx" ] && [ -f "$dest/decoder.onnx" ] \
       && [ -f "$dest/joiner.onnx" ]  && [ -f "$dest/tokens.txt" ]; then
      echo "streaming-zipformer model already present at $dest/"
      exit 0
    fi
    mkdir -p models
    stem="sherpa-onnx-streaming-zipformer-en-20M-2023-02-17"
    archive="$stem.tar.bz2"
    url="https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/$archive"
    tmp="$(mktemp -d)"
    trap "rm -rf '$tmp'" EXIT
    echo "downloading $archive …"
    curl -fL --progress-bar -o "$tmp/$archive" "$url"
    tar -xjf "$tmp/$archive" -C "$tmp"
    mkdir -p "$dest"
    cp "$tmp/$stem/encoder-epoch-99-avg-1.int8.onnx" "$dest/encoder.onnx"
    cp "$tmp/$stem/decoder-epoch-99-avg-1.onnx"      "$dest/decoder.onnx"
    cp "$tmp/$stem/joiner-epoch-99-avg-1.int8.onnx"  "$dest/joiner.onnx"
    cp "$tmp/$stem/tokens.txt"                       "$dest/tokens.txt"
    echo "installed streaming-zipformer model → $dest/"

# --- vox_scribe (standalone live transcription TUI) ---

# Download the speaker models for `scribe --diarize` into
# models/speaker-diarization/: pyannote segmentation 3.0 (~6 MB) and the
# 3D-Speaker ERes2NetV2 embedding model (~71 MB). `scribe
# --diarize` also fetches them on first use into its own models dir.
download-speaker-models:
    #!/usr/bin/env bash
    set -euo pipefail
    dest="models/speaker-diarization"
    emb="embedding-eres2netv2.onnx"
    if [ -f "$dest/segmentation.onnx" ] && [ -f "$dest/$emb" ]; then
      echo "speaker models already present at $dest/"
      exit 0
    fi
    base="https://github.com/k2-fsa/sherpa-onnx/releases/download"
    stem="sherpa-onnx-pyannote-segmentation-3-0"
    tmp="$(mktemp -d)"
    trap "rm -rf '$tmp'" EXIT
    echo "downloading $stem …"
    curl -fL --progress-bar -o "$tmp/$stem.tar.bz2" "$base/speaker-segmentation-models/$stem.tar.bz2"
    tar -xjf "$tmp/$stem.tar.bz2" -C "$tmp"
    mkdir -p "$dest"
    cp "$tmp/$stem/model.onnx" "$dest/segmentation.onnx"
    echo "downloading the speaker embedding model …"
    curl -fL --progress-bar -o "$dest/$emb.part" \
      "$base/speaker-recongition-models/3dspeaker_speech_eres2netv2_sv_zh-cn_16k-common.onnx"
    mv "$dest/$emb.part" "$dest/$emb"
    echo "installed speaker models → $dest/"

# Uses nix-shell when available (Linux), plain cargo otherwise (macOS,
# where only this crate builds).
# Run the vox_scribe live transcription TUI (fetches models first).
scribe *ARGS: download-stt-model download-streaming-stt-model
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v nix-shell >/dev/null; then
      nix-shell --run "cargo run -p vox_scribe --release -- {{ARGS}}"
    else
      cargo run -p vox_scribe --release -- {{ARGS}}
    fi

# Uses nix-shell when available (Linux), plain cargo otherwise (macOS).
# Optimized vox_scribe build only → target/release/scribe.
release-scribe:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v nix-shell >/dev/null; then
      nix-shell --run "cargo build -p vox_scribe --release"
    else
      cargo build -p vox_scribe --release
    fi

# Pi with the voice extension (see PI.md): talk to it, hear the replies.
pi *ARGS:
    pi -e pi/scribe-voice.ts {{ARGS}}

# On macOS: mlx-audio and the voice model, run locally. Elsewhere: only
# playback, for a Qwen3-TTS server set by PI_TTS_URL (see qwen3-tts/);
# sounddevice needs PortAudio (libportaudio2 / portaudio).
# Python venv for pi/pi-tts.py (see PI.md).
pi-tts-venv:
    #!/usr/bin/env bash
    set -euo pipefail
    uv venv --python 3.12 pi/.venv
    if [ "$(uname)" = Darwin ]; then
      uv pip install --python pi/.venv/bin/python "mlx-audio==0.6.2" soundfile sounddevice
      pi/.venv/bin/hf download mlx-community/Qwen3-TTS-12Hz-1.7B-CustomVoice-4bit
    else
      uv pip install --python pi/.venv/bin/python numpy soundfile sounddevice
      pi/.venv/bin/python -c "import sounddevice" \
        || echo "sounddevice can't find PortAudio: install libportaudio2 (or your distro's portaudio)" >&2
      [ -n "${PI_TTS_URL:-}" ] || echo "set PI_TTS_URL to your Qwen3-TTS server (see qwen3-tts/)" >&2
    fi

# Build vox_scribe, install it to BIN and download its models.
install-scribe BIN="~/.local/bin": release-scribe
    #!/usr/bin/env bash
    set -euo pipefail
    bin="{{BIN}}"
    bin="${bin/#\~/$HOME}"
    mkdir -p "$bin"
    install -m 755 target/release/scribe "$bin/scribe"
    echo "installed $bin/scribe"
    if [ "$(uname)" = Darwin ]; then
      contrib=crates/vox_scribe/contrib/macos
      sed "s|@BIN@|$bin|" "$contrib/scribe-once.terminal" > "$bin/scribe-once.terminal"
      install -m 755 "$contrib/scribe-ghostty" "$bin/"
      rules="$HOME/.config/karabiner/assets/complex_modifications"
      mkdir -p "$rules"
      # Ghostty runs as its own app, so only the recorder's window comes
      # forward; Terminal raises all of its windows. Override with
      # VOX_SCRIBE_TERMINAL=terminal|ghostty.
      term="${VOX_SCRIBE_TERMINAL:-}"
      if [ -z "$term" ]; then
        if [ -d /Applications/Ghostty.app ]; then term=ghostty; else term=terminal; fi
      fi
      case "$term" in
        ghostty) launch="$bin/scribe-ghostty" ;;
        terminal) launch="open -a Terminal $bin/scribe-once.terminal" ;;
        *) echo "VOX_SCRIBE_TERMINAL must be terminal or ghostty" >&2; exit 1 ;;
      esac
      sed -e "s|@BIN@|$bin|g" -e "s|@LAUNCH@|$launch|" "$contrib/karabiner-scribe.json" > "$rules/scribe.json"
      echo "installed the Karabiner rule ($term; enable it in Karabiner-Elements)"
    fi
    "$bin/scribe" download-models
    case ":$PATH:" in
      *":$bin:"*) ;;
      *) echo "note: $bin is not on your PATH" ;;
    esac

# --- Windows (x86_64) ---

# Cross-build scribe.exe (and agent.exe) for 64-bit Windows from this
# machine, with cargo-xwin: it fetches Microsoft's CRT and Windows SDK
# headers and libraries, and clang-cl / lld-link stand in for MSVC.
# Needs nix (for the LLVM tools) and rustup. Release only: a debug
# build wants the debug C runtime, which cargo-xwin doesn't fetch.
#
# Release builds happen natively on a Windows runner (.github/workflows/
# release.yml), which needs none of this; `just win64` is for trying a
# change without pushing one. Pass different packages as ARGS, e.g.
#   just win64 "-p vox_scribe"
win64 ARGS="-p vox_scribe -p vox_agent":
    #!/usr/bin/env bash
    set -euo pipefail
    target=x86_64-pc-windows-msvc
    if ! rustup target list --installed | grep -qx "$target"; then
      rustup target add "$target"
    fi
    # libopus's CMake tells MSVC that its SSE4.1 and AVX files need those
    # instruction sets, and says it in a way only MSVC understands;
    # clang-cl has to be told in its own words. `CL` goes in front of
    # every clang-cl command line.
    export CL="/clang:-msse4.1 /clang:-mavx"
    shell=(
      nix shell
      nixpkgs#cargo-xwin
      nixpkgs#llvmPackages.clang-unwrapped  # clang-cl
      nixpkgs#llvmPackages.llvm             # llvm-lib
      nixpkgs#lld                           # lld-link
      # audiopus_sys builds the libopus it bundles.
      nixpkgs#cmake
      nixpkgs#ninja
      --command
    )
    # Fetch the CRT and SDK before patching the names below.
    "${shell[@]}" cargo xwin cache xwin
    # cargo-xwin unpacks the SDK under lowercase, Capitalised and
    # UPPERCASE names and nothing else, so the mixed-case libraries that
    # onnxruntime asks for are missed on a case-sensitive filesystem.
    # Windows itself never notices the difference.
    um="${XWIN_CACHE_DIR:-$HOME/.cache/cargo-xwin}/xwin/sdk/lib/um/x86_64"
    for lib in PathCch; do
      lower="$(printf %s "$lib" | tr "[:upper:]" "[:lower:]")"
      if [ -e "$um/$lower.lib" ] && [ ! -e "$um/$lib.lib" ]; then
        ln -s "$lower.lib" "$um/$lib.lib"
      fi
    done
    "${shell[@]}" cargo xwin build --release --locked --target "$target" {{ARGS}}
    ls -l "target/$target/release/"*.exe

# Android arm64 (API 26+) scribe bundle in dist/android-arm64/: scribe, the
# sherpa-onnx / onnxruntime .so files it loads, and run.sh, which sets
# LD_LIBRARY_PATH and points VOX_SCRIBE_MODELS at ./models unless already
# set. Needs the NDK (ANDROID_NDK_HOME, default: Homebrew's android-ndk),
# `rustup target add aarch64-linux-android` and `cargo install cargo-ndk`.
android:
    #!/usr/bin/env bash
    set -euo pipefail
    export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-/opt/homebrew/share/android-ndk}"
    # libopus is built by CMake, which needs the ABI and API level pinned.
    export CMAKE_TOOLCHAIN_FILE="$PWD/android/toolchain.cmake"
    # Its own target dir, like ios-app: sherpa-onnx-sys caches prebuilt libs
    # per target dir, and its Android jniLibs/ shadow every other target's
    # lib dir (the build script checks that path whatever the target is, so
    # a host build then looks for .a files among Android .so files).
    export CARGO_TARGET_DIR="$PWD/target/android"
    cargo ndk -t arm64-v8a --platform 26 build --release -p vox_scribe
    out=dist/android-arm64
    rm -rf "$out"
    mkdir -p "$out"
    libs="$CARGO_TARGET_DIR/sherpa-onnx-prebuilt/jniLibs/arm64-v8a"
    cp "$CARGO_TARGET_DIR/aarch64-linux-android/release/scribe" "$out/"
    cp "$libs/libsherpa-onnx-c-api.so" "$libs/libonnxruntime.so" "$out/"
    cp SCRIBE.md "$out/"
    printf '%s\n' '#!/system/bin/sh' \
      'dir="$(cd "$(dirname "$0")" && pwd)"' \
      'export LD_LIBRARY_PATH="$dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"' \
      'export VOX_SCRIBE_MODELS="${VOX_SCRIBE_MODELS:-$dir/models}"' \
      'exec "$dir/scribe" "$@"' > "$out/run.sh"
    chmod 755 "$out/run.sh" "$out/scribe"
    readelf="$(ls "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-readelf | head -1)"
    for need in $("$readelf" -d "$out"/scribe "$out"/*.so | sed -n 's/.*NEEDED.*\[\(.*\)\]/\1/p' | sort -u); do
      case "$need" in
        libc.so|libm.so|libdl.so|liblog.so|libandroid.so|libaaudio.so) ;;
        *) [ -f "$out/$need" ] || { echo "missing $need" >&2; exit 1; } ;;
      esac
    done
    du -sh "$out"

# Android dictation app (android/): builds crates/vox_android into its
# jniLibs next to the sherpa-onnx libs, assembles the debug APK and, with
# a device on adb, installs it. Needs what `just android` needs plus a
# JDK 17+ (JAVA_HOME, default: Homebrew's openjdk@21) and the Android SDK
# (ANDROID_HOME, default: Homebrew's android-commandlinetools). Signs with
# the release key, SCRIBE_KEYSTORE (default ~/scribe.jks), when it exists,
# with SCRIBE_KEYSTORE_PASSWORD (e.g. in .env), else "android", else asking,
# so it installs over a release; the debug key otherwise.
android-app INSTALL="yes": _android-jni
    #!/usr/bin/env bash
    set -euo pipefail
    export ANDROID_HOME="${ANDROID_HOME:-/opt/homebrew/share/android-commandlinetools}"
    export JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@21}"
    if [ -z "${SCRIBE_KEYSTORE:-}" ] && [ -f "$HOME/scribe.jks" ]; then
      export SCRIBE_KEYSTORE="$HOME/scribe.jks"
    fi
    if [ -n "${SCRIBE_KEYSTORE:-}" ] && [ -z "${SCRIBE_KEYSTORE_PASSWORD:-}" ]; then
      export SCRIBE_KEYSTORE_PASSWORD=android
      if ! "$JAVA_HOME/bin/keytool" -list -keystore "$SCRIBE_KEYSTORE" \
          -storepass:env SCRIBE_KEYSTORE_PASSWORD >/dev/null 2>&1; then
        read -rsp "Password for $SCRIBE_KEYSTORE: " SCRIBE_KEYSTORE_PASSWORD; echo
      fi
    fi
    [ -f android/local.properties ] || echo "sdk.dir=$ANDROID_HOME" > android/local.properties
    (cd android && ./gradlew assembleDebug -PscribeVersionName="$(git describe --tags --always --dirty)")
    apk=android/app/build/outputs/apk/debug/app-debug.apk
    du -h "$apk"
    if [ "{{INSTALL}}" = yes ]; then
      if adb get-state >/dev/null 2>&1; then
        adb install -r "$apk"
      else
        echo "no device on adb, so not installed (USB or wireless debugging; check \`adb devices\`)" >&2
      fi
    fi

# Release APK of the dictation app in dist/, as the release workflow ships
# it. Signed with the keystore in SCRIBE_KEYSTORE (plus SCRIBE_KEYSTORE_PASSWORD,
# SCRIBE_KEY_ALIAS, SCRIBE_KEY_PASSWORD) when set, the debug key otherwise.
android-apk VERSION=`git describe --tags --always --dirty`: _android-jni
    #!/usr/bin/env bash
    set -euo pipefail
    export ANDROID_HOME="${ANDROID_HOME:-/opt/homebrew/share/android-commandlinetools}"
    export JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@21}"
    [ -f android/local.properties ] || echo "sdk.dir=$ANDROID_HOME" > android/local.properties
    (cd android && ./gradlew assembleRelease \
      -PscribeVersionName="{{VERSION}}")
    name="scribe-{{VERSION}}-android-arm64.apk"
    mkdir -p dist
    cp android/app/build/outputs/apk/release/app-release.apk "dist/$name"
    (cd dist && shasum -a 256 "$name" > "$name.sha256")
    echo "packaged dist/$name"

# Release signing key for the APK: creates KEYSTORE (asks for a password)
# unless it exists, then, after confirmation, stores it and its password as
# the repo secrets the release workflow signs with (needs `gh`). Every APK
# must be signed by this same key to install over the last one. GitHub
# can't give the secret back, so a lost KEYSTORE only matters if the
# secret goes too: a new key then means uninstalling Scribe once.
android-keystore KEYSTORE="":
    #!/usr/bin/env bash
    set -euo pipefail
    export JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@21}"
    keytool="$JAVA_HOME/bin/keytool"
    keystore="{{KEYSTORE}}"
    keystore="${keystore:-$HOME/scribe.jks}"
    alias=scribe
    read -rsp "Keystore password: " SCRIBE_KEYSTORE_PASSWORD; echo
    export SCRIBE_KEYSTORE_PASSWORD
    if [ -f "$keystore" ]; then
      "$keytool" -list -keystore "$keystore" -storepass:env SCRIBE_KEYSTORE_PASSWORD -alias "$alias" >/dev/null \
        || { echo "wrong password, or no $alias key in $keystore" >&2; exit 1; }
      echo "using existing $keystore"
    else
      [ "${#SCRIBE_KEYSTORE_PASSWORD}" -ge 6 ] || { echo "password needs 6+ characters" >&2; exit 1; }
      read -rsp "Again: " again; echo
      [ "$again" = "$SCRIBE_KEYSTORE_PASSWORD" ] || { echo "passwords differ" >&2; exit 1; }
      # PKCS12 keystores use the store password for the key too.
      "$keytool" -genkeypair -keystore "$keystore" -alias "$alias" \
        -keyalg RSA -keysize 4096 -validity 10000 -dname CN=Scribe \
        -storepass:env SCRIBE_KEYSTORE_PASSWORD
      chmod 600 "$keystore"
      echo "created $keystore; back it up"
    fi
    read -rp "Store it as secrets of $(gh repo view --json nameWithOwner -q .nameWithOwner)? [y/N] " ok
    [ "$ok" = y ] || [ "$ok" = Y ] || exit 0
    base64 < "$keystore" | gh secret set SCRIBE_KEYSTORE_BASE64
    printf %s "$SCRIBE_KEYSTORE_PASSWORD" | gh secret set SCRIBE_KEYSTORE_PASSWORD
    printf %s "$SCRIBE_KEYSTORE_PASSWORD" | gh secret set SCRIBE_KEY_PASSWORD
    printf %s "$alias" | gh secret set SCRIBE_KEY_ALIAS

# libvox_android.so and the sherpa-onnx libs it loads, into the app's jniLibs.
_android-jni:
    #!/usr/bin/env bash
    set -euo pipefail
    export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-/opt/homebrew/share/android-ndk}"
    export CMAKE_TOOLCHAIN_FILE="$PWD/android/toolchain.cmake"
    # Its own target dir; see the `android` recipe for why.
    export CARGO_TARGET_DIR="$PWD/target/android"
    cargo ndk -t arm64-v8a --platform 26 build --release --locked -p vox_android
    jni=android/app/src/main/jniLibs/arm64-v8a
    libs="$CARGO_TARGET_DIR/sherpa-onnx-prebuilt/jniLibs/arm64-v8a"
    mkdir -p "$jni"
    cp "$CARGO_TARGET_DIR/aarch64-linux-android/release/libvox_android.so" \
      "$libs/libsherpa-onnx-c-api.so" "$libs/libonnxruntime.so" "$jni/"

# iOS dictation app (ios/): builds crates/vox_ios for arm64 devices,
# generates ios/Scribe.xcodeproj (xcodegen), builds and signs it and, with
# INSTALL=yes, installs and launches it on SCRIBE_DEVICE (name or UDID;
# default: the first paired iPhone/iPad). Needs Xcode signed in to an
# Apple ID (a free Personal Team works), `brew install xcodegen` and
# `rustup target add aarch64-apple-ios`. SCRIBE_TEAM defaults to the team
# of the first Apple Development certificate; SCRIBE_BUNDLE_ID to
# com.enigmacurry.rpg-vox-scribe (a free team may need its own).
ios-app INSTALL="yes":
    #!/usr/bin/env bash
    set -euo pipefail
    source <(just _ios-xcode)
    # Its own target dir: sherpa-onnx-sys caches prebuilt libs per target
    # dir and would take the Android ones for iOS.
    export CARGO_TARGET_DIR="$PWD/target/ios"
    export IPHONEOS_DEPLOYMENT_TARGET=17.0
    cargo build --release --locked -p vox_ios --target aarch64-apple-ios
    mkdir -p ios/Frameworks
    rsync -a --delete "$CARGO_TARGET_DIR"/sherpa-onnx-prebuilt/sherpa-onnx-v*-ios-shared-onnxruntime-static/SherpaOnnxC.xcframework/ \
      ios/Frameworks/SherpaOnnxC.xcframework/
    team="${SCRIBE_TEAM:-$(security find-certificate -c "Apple Development" -p 2>/dev/null \
      | openssl x509 -noout -subject -nameopt multiline 2>/dev/null \
      | sed -n 's/^ *organizationalUnitName *= *//p' | head -1)}"
    [ -n "$team" ] || { echo "no signing team: sign in under Xcode > Settings > Accounts, or set SCRIBE_TEAM" >&2; exit 1; }
    bundle="${SCRIBE_BUNDLE_ID:-com.enigmacurry.rpg-vox-scribe}"
    # Building for the device registers it with the team, which a free
    # team's provisioning profile needs.
    destination=generic/platform=iOS
    if [ "{{INSTALL}}" = yes ]; then
      device="$(just _ios-device)"
      destination="id=$device"
    fi
    xcodegen generate --quiet --spec ios/project.yml
    xcodebuild -quiet -project ios/Scribe.xcodeproj -scheme Scribe -configuration Release \
      -destination "$destination" -derivedDataPath ios/build -allowProvisioningUpdates -allowProvisioningDeviceRegistration \
      DEVELOPMENT_TEAM="$team" PRODUCT_BUNDLE_IDENTIFIER="$bundle" \
      MARKETING_VERSION="$(git describe --tags --abbrev=0 | sed 's/^v//')" build
    app=ios/build/Build/Products/Release-iphoneos/Scribe.app
    du -sh "$app"
    [ "{{INSTALL}}" = yes ] || exit 0
    xcrun devicectl device install app --device "$device" "$app"
    xcrun devicectl device process launch --device "$device" --terminate-existing "$bundle"

# The iOS app in the Simulator (DEVICE, default an iPad Pro), booting
# it if needed, with the models copied from this Mac's Scribe models dir
# the first time. Needs `rustup target add aarch64-apple-ios-sim`.
ios-sim DEVICE="iPad Pro 11-inch (M5)":
    #!/usr/bin/env bash
    set -euo pipefail
    source <(just _ios-xcode)
    export CARGO_TARGET_DIR="$PWD/target/ios"
    export IPHONEOS_DEPLOYMENT_TARGET=17.0
    cargo build --release --locked -p vox_ios --target aarch64-apple-ios-sim
    mkdir -p ios/Frameworks
    rsync -a --delete "$CARGO_TARGET_DIR"/sherpa-onnx-prebuilt/sherpa-onnx-v*-ios-shared-onnxruntime-static/SherpaOnnxC.xcframework/ \
      ios/Frameworks/SherpaOnnxC.xcframework/
    bundle="${SCRIBE_BUNDLE_ID:-com.enigmacurry.rpg-vox-scribe}"
    xcodegen generate --quiet --spec ios/project.yml
    xcodebuild -quiet -project ios/Scribe.xcodeproj -scheme Scribe -configuration Release \
      -destination "platform=iOS Simulator,name={{DEVICE}}" -derivedDataPath ios/build \
      PRODUCT_BUNDLE_IDENTIFIER="$bundle" CODE_SIGNING_ALLOWED=NO build
    xcrun simctl boot "{{DEVICE}}" 2>/dev/null || true
    xcrun simctl install "{{DEVICE}}" ios/build/Build/Products/Release-iphonesimulator/Scribe.app
    data="$(xcrun simctl get_app_container "{{DEVICE}}" "$bundle" data)"
    if [ ! -d "$data/Library/Application Support/models/parakeet-tdt-0.6b-v2" ]; then
      mkdir -p "$data/Documents/models"
      cp -R "$HOME/Library/Application Support/vox_scribe/models/parakeet-tdt-0.6b-v2" \
        "$HOME/Library/Application Support/vox_scribe/models/streaming-zipformer" "$data/Documents/models/"
    fi
    xcrun simctl launch --terminate-running-process "{{DEVICE}}" "$bundle"

# Copy Parakeet and the streaming Zipformer from MODELS (default: Scribe's
# models dir on this Mac) into the iOS app's Documents/models on
# SCRIBE_DEVICE. The app must be installed; relaunch it afterwards.
ios-models MODELS="":
    #!/usr/bin/env bash
    set -euo pipefail
    models="{{MODELS}}"
    models="${models:-$HOME/Library/Application Support/vox_scribe/models}"
    bundle="${SCRIBE_BUNDLE_ID:-com.enigmacurry.rpg-vox-scribe}"
    source <(just _ios-xcode)
    device="$(just _ios-device)"
    for m in parakeet-tdt-0.6b-v2 streaming-zipformer; do
      for f in "$models/$m"/*; do
        echo "$m/$(basename "$f")"
        xcrun devicectl device copy to --device "$device" --domain-type appDataContainer \
          --domain-identifier "$bundle" --source "$f" --destination "Documents/models/$m/$(basename "$f")"
      done
    done

# Exports DEVELOPER_DIR=/Applications/Xcode.app/... when xcode-select
# still points at the Command Line Tools, which have no iOS SDK.
_ios-xcode:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! xcodebuild -version >/dev/null 2>&1; then
      [ -d /Applications/Xcode.app ] || { echo "needs Xcode from the App Store" >&2; exit 1; }
      echo "export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer"
    fi

# SCRIBE_DEVICE, else the first paired iPhone/iPad devicectl lists.
_ios-device:
    #!/usr/bin/env bash
    set -euo pipefail
    source <(just _ios-xcode)
    if [ -n "${SCRIBE_DEVICE:-}" ]; then echo "$SCRIBE_DEVICE"; exit 0; fi
    json="$(mktemp)"
    trap 'rm -f "$json"' EXIT
    xcrun devicectl list devices --quiet --json-output "$json" >/dev/null
    python3 -I - "$json" <<'PY'
    import json, sys
    devs = json.load(open(sys.argv[1]))["result"]["devices"]
    for d in devs:
        hw = d.get("hardwareProperties", {})
        if hw.get("platform") == "iOS" and hw.get("reality") != "simulated" \
                and d.get("connectionProperties", {}).get("pairingState") == "paired":
            print(d["hardwareProperties"]["udid"]); break
    else:
        sys.exit("no paired iOS device: connect it by USB, unlock it, tap Trust, then `xcrun devicectl manage pair --device <udid>` (or set SCRIBE_DEVICE)")
    PY

# --- discord_vox (PipeWire sink → Discord voice) ---

# Run the discord_vox release binary. Expects DISCORD_VOX_TOKEN /
# DISCORD_VOX_GUILD_ID / DISCORD_VOX_CHANNEL_ID in the env (or pass --token / --guild-id / --channel-id).
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

# --- vox_agent (LLM responder for scribe --chat) ---

# Run agent on a scribe --chat conversation, e.g. `just agent talk`.
agent *ARGS:
    cargo run -p vox_agent --release -- {{ARGS}}

# Build agent and install it to BIN.
install-agent BIN="~/.local/bin":
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -p vox_agent --release
    bin="{{BIN}}"
    bin="${bin/#\~/$HOME}"
    mkdir -p "$bin"
    install -m 755 target/release/agent "$bin/agent"
    echo "installed $bin/agent"

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
    mkdir -p dist
    for bin in "${bins[@]}"; do
      name="$bin-{{VERSION}}-$target"
      stage="$(mktemp -d)/$name"
      mkdir -p "$stage"
      cp "target/release/$bin" LICENSE.txt README.md "$stage/"
      case "$bin" in
        scribe) cp SCRIBE.md SCRIBE_REFERENCE.md "$stage/" ;;
        agent) cp AGENT.md "$stage/" ;;
        rpg_vox) cp RPG_VOX.md "$stage/" ;;
      esac
      tar -czf "dist/$name.tar.gz" -C "$(dirname "$stage")" "$name"
      rm -rf "$(dirname "$stage")"
      (cd dist && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256")
      echo "packaged dist/$name.tar.gz"
    done

# Cut a release from master: bump the workspace version (prompts, default
# next patch), commit, tag v<version> and push both after confirmation.
# The tag push runs the release workflow. Needs a clean, up-to-date master
# (untracked files are ignored).
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
    cargo update --workspace --quiet
    git --no-pager diff --stat
    read -rp "Commit, tag $tag and push to origin? [y/N] " ok
    if [ "$ok" != y ] && [ "$ok" != Y ]; then
      git checkout -- Cargo.toml Cargo.lock
      echo "aborted; version change reverted"
      exit 1
    fi
    git commit -q -m "Release $tag" Cargo.toml Cargo.lock
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
    cargo ndk -t arm64-v8a --platform 26 build --release -p vox_scribe
    out=dist/android-arm64
    rm -rf "$out"
    mkdir -p "$out"
    libs=target/sherpa-onnx-prebuilt/jniLibs/arm64-v8a
    cp target/aarch64-linux-android/release/scribe "$out/"
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
# (ANDROID_HOME, default: Homebrew's android-commandlinetools).
android-app INSTALL="yes": _android-jni
    #!/usr/bin/env bash
    set -euo pipefail
    export ANDROID_HOME="${ANDROID_HOME:-/opt/homebrew/share/android-commandlinetools}"
    export JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@21}"
    [ -f android/local.properties ] || echo "sdk.dir=$ANDROID_HOME" > android/local.properties
    (cd android && ./gradlew assembleDebug)
    apk=android/app/build/outputs/apk/debug/app-debug.apk
    du -h "$apk"
    if [ "{{INSTALL}}" = yes ] && adb get-state >/dev/null 2>&1; then
      adb install -r "$apk"
    fi

# Release APK of the dictation app in dist/, as the release workflow ships
# it. Signed with the keystore in SCRIBE_KEYSTORE (plus SCRIBE_KEYSTORE_PASSWORD,
# SCRIBE_KEY_ALIAS, SCRIBE_KEY_PASSWORD) when set, the debug key otherwise.
# VERSION_CODE must grow between releases for Android to upgrade in place.
android-apk VERSION=`git describe --tags --always --dirty` VERSION_CODE="1": _android-jni
    #!/usr/bin/env bash
    set -euo pipefail
    export ANDROID_HOME="${ANDROID_HOME:-/opt/homebrew/share/android-commandlinetools}"
    export JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@21}"
    [ -f android/local.properties ] || echo "sdk.dir=$ANDROID_HOME" > android/local.properties
    (cd android && ./gradlew assembleRelease \
      -PscribeVersionName="{{VERSION}}" -PscribeVersionCode="{{VERSION_CODE}}")
    name="scribe-{{VERSION}}-android-arm64.apk"
    mkdir -p dist
    cp android/app/build/outputs/apk/release/app-release.apk "dist/$name"
    (cd dist && shasum -a 256 "$name" > "$name.sha256")
    echo "packaged dist/$name"

# Release signing key for the APK: creates KEYSTORE (asks for a password)
# unless it exists, then, after confirmation, stores it and its password as
# the repo secrets the release workflow signs with (needs `gh`). Every APK
# must be signed by this same key to install over the last one, so back
# KEYSTORE up; losing it means uninstalling Scribe to update.
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
    cargo ndk -t arm64-v8a --platform 26 build --release --locked -p vox_android
    jni=android/app/src/main/jniLibs/arm64-v8a
    libs=target/sherpa-onnx-prebuilt/jniLibs/arm64-v8a
    mkdir -p "$jni"
    cp target/aarch64-linux-android/release/libvox_android.so \
      "$libs/libsherpa-onnx-c-api.so" "$libs/libonnxruntime.so" "$jni/"

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

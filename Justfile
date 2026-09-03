# rpg_vox task runner. All recipes run inside the project's nix-shell.

# Default: show recipes.
default:
    @just --list

# Interactive dev shell with rustc, cargo, pipewire headers, etc.
shell:
    nix-shell

# Debug build.
build:
    nix-shell --run "cargo build"

# Optimized build (used for actually running the bridge).
release:
    nix-shell --run "cargo build --release"

# Fast type-check without codegen.
check:
    nix-shell --run "cargo check"

# Lints, fail on warnings.
clippy:
    nix-shell --run "cargo clippy --all-targets -- -D warnings"

# Format.
fmt:
    nix-shell --run "cargo fmt"

# Run the release binary. Extra args are forwarded, e.g.
#   just run -- --comfyui http://192.168.1.10:8188
run *ARGS:
    nix-shell --run "cargo run --release -- {{ARGS}}"

# Run the debug binary (faster to rebuild while iterating).
dev *ARGS:
    nix-shell --run "cargo run -- {{ARGS}}"

# Tests.
test:
    nix-shell --run "cargo test"

# Post text to a running rpg_vox instance.
#   just say "hello world"
say TEXT bind="127.0.0.1:7331":
    curl -sS -X POST http://{{bind}}/say \
        -H 'content-type: application/json' \
        --data-binary @<(jq -Rn --arg t "{{TEXT}}" '{text:$t}')
    @echo

# Verify the source node is visible to PipeWire.
list-sources:
    pw-cli list-objects Node | grep -A1 -B1 "rpg-vox" || echo "rpg-vox not found — is the bridge running?"

# Clean build artifacts.
clean:
    cargo clean

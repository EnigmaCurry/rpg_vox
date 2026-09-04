{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    rustc
    cargo
    cargo-watch
    rustfmt
    clippy
    pkg-config
    clang
    # espeak-rs-sys (pulled in by piper-rs) builds espeak-ng from vendored source.
    cmake
    # Web UI: Vite + Svelte SPA built by build.rs and embedded via rust-embed.
    nodejs_22
    pnpm
  ];

  buildInputs = with pkgs; [
    pipewire
    pipewire.dev
    openssl
    openssl.dev
    libopus
    # Runtime dep of discord_vox --record: WAV → FLAC (mixed) and
    # WAV → Opus (per-user) transcode happens via `ffmpeg` at session end.
    ffmpeg
    # `ort` (ONNX Runtime bindings, load-dynamic) dlopens this at runtime;
    # ORT_DYLIB_PATH below points it at the nixpkgs build.
    onnxruntime
  ];

  # bindgen (used by pipewire-sys) needs libclang at runtime.
  LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

  # Help bindgen find system headers.
  BINDGEN_EXTRA_CLANG_ARGS =
    "-I${pkgs.glibc.dev}/include -I${pkgs.pipewire.dev}/include/pipewire-0.3 -I${pkgs.pipewire.dev}/include/spa-0.2";

  # Point `ort` at nixpkgs' libonnxruntime instead of its bundled download.
  ORT_DYLIB_PATH = "${pkgs.onnxruntime}/lib/libonnxruntime.so";

  shellHook = ''
    echo "rpg_vox dev shell: rustc $(rustc --version), pipewire $(${pkgs.pipewire}/bin/pipewire --version | head -1)"
    echo "ORT_DYLIB_PATH=$ORT_DYLIB_PATH"
  '';
}

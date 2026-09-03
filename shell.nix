{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    rustc
    cargo
    rustfmt
    clippy
    pkg-config
    clang
  ];

  buildInputs = with pkgs; [
    pipewire
    pipewire.dev
    openssl
    openssl.dev
  ];

  # bindgen (used by pipewire-sys) needs libclang at runtime.
  LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

  # Help bindgen find system headers.
  BINDGEN_EXTRA_CLANG_ARGS =
    "-I${pkgs.glibc.dev}/include -I${pkgs.pipewire.dev}/include/pipewire-0.3 -I${pkgs.pipewire.dev}/include/spa-0.2";

  shellHook = ''
    echo "rpg_vox dev shell: rustc $(rustc --version), pipewire $(${pkgs.pipewire}/bin/pipewire --version | head -1)"
  '';
}

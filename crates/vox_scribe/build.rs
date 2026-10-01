// sherpa-onnx-sys copies its shared libraries next to the binary, but a
// dependency's `rustc-link-arg` never reaches the final link, so the
// binary itself has to carry the relative rpath.
fn main() {
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@loader_path"),
        Ok("linux") => println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN"),
        _ => {}
    }
}

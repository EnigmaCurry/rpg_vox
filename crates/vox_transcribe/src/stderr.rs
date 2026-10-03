//! Process-wide stderr filter that drops sherpa-onnx's noisy C++ log lines
//! without affecting Rust `tracing` output.
//!
//! Sherpa-onnx's C++ code writes directly via `fprintf(stderr, …)` and has
//! no runtime knob to silence it. In particular `AcceptWaveformImpl` logs
//!
//! ```text
//! …/sherpa-onnx/csrc/offline-stream.cc:AcceptWaveformImpl:142 Creating a resampler:
//!    in_sample_rate: 48000
//!    output_sample_rate: 16000
//! ```
//!
//! on every offline decode, which floods the terminal during recording.
//!
//! The fix: replace fd 2 with the write end of a pipe, save the original
//! fd, and run a background thread that reads pipe lines, drops the ones
//! that match the sherpa banner (plus the two indented follow-ups), and
//! forwards the rest to the saved original stderr. Must be called BEFORE
//! `tracing_subscriber` grabs a handle to stderr, otherwise tracing keeps
//! writing to the pre-swap fd.
//!
//! Unix only.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::sync::OnceLock;

/// The terminal's stderr from before [`install`] or [`redirect_to`]
/// swapped fd 2, for output that must reach the user directly: progress
/// bars redrawn with `\r` (the filter forwards whole lines only), or
/// status lines printed while fd 2 points at a log file.
static CONSOLE: OnceLock<std::fs::File> = OnceLock::new();

/// A handle on the original stderr, if fd 2 has been swapped.
pub fn console() -> Option<std::fs::File> {
    CONSOLE.get().and_then(|f| f.try_clone().ok())
}

/// Remember a dup of the current fd 2 as [`console`] (first call wins).
fn save_console() {
    // SAFETY: dup of the well-known stderr fd; the new fd is ours.
    unsafe {
        let fd = libc::dup(libc::STDERR_FILENO);
        if fd >= 0 && CONSOLE.set(std::fs::File::from_raw_fd(fd)).is_err() {
            libc::close(fd);
        }
    }
}

/// Install the filter. Returns whether the *original* stderr was a TTY,
/// so callers configuring loggers can still enable ANSI colors.
pub fn install() -> bool {
    save_console();
    // SAFETY: syscalls guarded by return-value checks; we only touch fds
    // we just created or just duplicated.
    unsafe {
        let was_tty = libc::isatty(libc::STDERR_FILENO) == 1;
        let orig_stderr = libc::dup(libc::STDERR_FILENO);
        if orig_stderr < 0 {
            return was_tty;
        }
        let mut pipe_fds: [libc::c_int; 2] = [0, 0];
        if libc::pipe(pipe_fds.as_mut_ptr()) != 0 {
            libc::close(orig_stderr);
            return was_tty;
        }
        let read_fd = pipe_fds[0];
        let write_fd = pipe_fds[1];
        if libc::dup2(write_fd, libc::STDERR_FILENO) < 0 {
            libc::close(read_fd);
            libc::close(write_fd);
            libc::close(orig_stderr);
            return was_tty;
        }
        libc::close(write_fd);

        let read_file = std::fs::File::from_raw_fd(read_fd);
        let orig_file = std::fs::File::from_raw_fd(orig_stderr);
        std::thread::Builder::new()
            .name("stderr-filter".into())
            .spawn(move || run(read_file, orig_file))
            .ok();

        was_tty
    }
}

/// Point fd 2 at `file` until the returned guard drops. A full-screen
/// TUI uses this so C++ log lines can't scribble over the display, then
/// restores the terminal so later errors are visible.
pub fn redirect_to(file: &std::fs::File) -> std::io::Result<RestoreStderr> {
    save_console();
    // SAFETY: dup/dup2 on fds we own or the well-known stderr fd.
    unsafe {
        let saved = libc::dup(libc::STDERR_FILENO);
        if saved < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO) < 0 {
            libc::close(saved);
            return Err(std::io::Error::last_os_error());
        }
        Ok(RestoreStderr(saved))
    }
}

/// Restores the original fd 2 on drop.
pub struct RestoreStderr(libc::c_int);

impl Drop for RestoreStderr {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the dup of the original stderr we own.
        unsafe {
            libc::dup2(self.0, libc::STDERR_FILENO);
            libc::close(self.0);
        }
    }
}

fn run(read_file: std::fs::File, mut orig_stderr: std::fs::File) {
    let mut reader = BufReader::new(read_file);
    // After we drop a sherpa banner we keep dropping subsequent indented
    // or blank lines. The concrete payload we're eating is:
    //
    //     …/offline-stream.cc:…:142 Creating a resampler:\n
    //     "   in_sample_rate: 48000\n"
    //     "   output_sample_rate: 16000\n"
    //     "\n"    ← SHERPA_ONNX_LOGE's own trailing fprintf(stderr, "\n")
    //
    // Cap the run at 4 lines so a bug in the pattern can't ever eat
    // arbitrary trailing output from a real logger.
    let mut trailing_to_drop: u32 = 0;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if is_sherpa_resampler_banner(&line) {
                    trailing_to_drop = 4;
                    continue;
                }
                if trailing_to_drop > 0 && (starts_with_indent(&line) || is_blank(&line)) {
                    trailing_to_drop -= 1;
                    continue;
                }
                trailing_to_drop = 0;
                let _ = orig_stderr.write_all(line.as_bytes());
            }
            Err(_) => break,
        }
    }
}

fn is_sherpa_resampler_banner(line: &str) -> bool {
    line.contains("sherpa-onnx") && line.contains("Creating a resampler")
}

fn starts_with_indent(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

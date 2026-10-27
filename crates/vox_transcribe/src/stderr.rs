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
//! [`redirect_to`] works on Windows too, which is what a TUI needs; the
//! filter above does not, and is unix-only. The forwarding thread can
//! only pass a line on once it has been scheduled, and on Windows a
//! process that writes its last words and exits — `Error: …`, say —
//! is gone before that happens, so they are never seen. Nothing but
//! sherpa's banner is lost by leaving the filter off.
//!
//! What makes either reach the C++ side at all is that sherpa-onnx is
//! linked into this binary and shares its C runtime, so its `stderr` is
//! the same descriptor 2 that [`sys`] dups about.

#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
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
    let fd = sys::dup(sys::STDERR_FD);
    if fd < 0 {
        return;
    }
    // A later call's dup is closed again when its File is dropped.
    let _ = CONSOLE.set(sys::file_from_fd(fd));
}

/// Install the filter. Returns whether the *original* stderr was a TTY,
/// so callers configuring loggers can still enable ANSI colors.
///
/// Unix only; see the module docs.
#[cfg(unix)]
pub fn install() -> bool {
    save_console();
    let was_tty = sys::isatty(sys::STDERR_FD);
    let orig_stderr = sys::dup(sys::STDERR_FD);
    if orig_stderr < 0 {
        return was_tty;
    }
    let Some((read_fd, write_fd)) = sys::pipe() else {
        sys::close(orig_stderr);
        return was_tty;
    };
    if sys::dup2(write_fd, sys::STDERR_FD) < 0 {
        sys::close(read_fd);
        sys::close(write_fd);
        sys::close(orig_stderr);
        return was_tty;
    }
    // fd 2 is the write end now; this one isn't needed.
    sys::close(write_fd);

    let read_file = sys::file_from_fd(read_fd);
    let orig_file = sys::file_from_fd(orig_stderr);
    std::thread::Builder::new()
        .name("stderr-filter".into())
        .spawn(move || run(read_file, orig_file))
        .ok();

    was_tty
}

/// Point fd 2 at `file` until the returned guard drops. A full-screen
/// TUI uses this so C++ log lines can't scribble over the display, then
/// restores the terminal so later errors are visible.
pub fn redirect_to(file: &std::fs::File) -> std::io::Result<RestoreStderr> {
    save_console();
    let saved = sys::dup(sys::STDERR_FD);
    if saved < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let fd = sys::fd_for_writes(file);
    if fd < 0 || sys::dup2(fd, sys::STDERR_FD) < 0 {
        let e = std::io::Error::last_os_error();
        sys::close(saved);
        return Err(e);
    }
    Ok(RestoreStderr(saved))
}

/// Keep stdout for the caller alone: returns a handle on the original
/// fd 1 and points fd 1 at stderr, so a stray `println!` (or C++ code
/// writing to stdout) lands with the logs instead of in the middle of a
/// machine-read stream.
pub fn take_stdout() -> std::io::Result<std::fs::File> {
    let saved = sys::dup(sys::STDOUT_FD);
    if saved < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if sys::dup2(sys::STDERR_FD, sys::STDOUT_FD) < 0 {
        let e = std::io::Error::last_os_error();
        sys::close(saved);
        return Err(e);
    }
    Ok(sys::file_from_fd(saved))
}

/// Restores the original fd 2 on drop.
pub struct RestoreStderr(sys::Fd);

impl Drop for RestoreStderr {
    fn drop(&mut self) {
        sys::dup2(self.0, sys::STDERR_FD);
        sys::close(self.0);
    }
}

/// The C runtime's descriptor plumbing, which differs between the two
/// platforms only in how it is spelled and in who owns what.
#[cfg(unix)]
mod sys {
    pub type Fd = libc::c_int;

    pub const STDERR_FD: Fd = libc::STDERR_FILENO;
    pub const STDOUT_FD: Fd = libc::STDOUT_FILENO;

    pub fn dup(fd: Fd) -> Fd {
        // SAFETY: a dup of a descriptor we were given; the new one is ours.
        unsafe { libc::dup(fd) }
    }

    pub fn dup2(from: Fd, to: Fd) -> Fd {
        // SAFETY: both are descriptors we own or the well-known stderr.
        unsafe { libc::dup2(from, to) }
    }

    pub fn pipe() -> Option<(Fd, Fd)> {
        let mut fds: [Fd; 2] = [0, 0];
        // SAFETY: `fds` is ours and big enough; the result is checked.
        let made = unsafe { libc::pipe(fds.as_mut_ptr()) } == 0;
        made.then_some((fds[0], fds[1]))
    }

    pub fn isatty(fd: Fd) -> bool {
        // SAFETY: a descriptor number, which is all isatty reads.
        unsafe { libc::isatty(fd) == 1 }
    }

    pub fn close(fd: Fd) {
        // SAFETY: a descriptor we own and don't use again.
        unsafe { libc::close(fd) };
    }

    /// Wraps `fd`, which the returned file then owns.
    pub fn file_from_fd(fd: Fd) -> std::fs::File {
        use std::os::unix::io::FromRawFd;
        // SAFETY: `fd` is ours, and we hand our only claim on it over.
        unsafe { std::fs::File::from_raw_fd(fd) }
    }

    /// A descriptor that writes into `file`, which the caller must not
    /// close: here it is the file's own.
    pub fn fd_for_writes(file: &std::fs::File) -> Fd {
        use std::os::unix::io::AsRawFd;
        file.as_raw_fd()
    }
}

#[cfg(windows)]
mod sys {
    use std::ffi::c_int;

    pub type Fd = c_int;

    pub const STDERR_FD: Fd = 2;
    pub const STDOUT_FD: Fd = 1;

    /// `_O_BINARY`: nothing is to be done to the newlines on the way
    /// through to the log file.
    const O_BINARY: c_int = 0x8000;

    // The C runtime's descriptors — where sherpa-onnx's
    // `fprintf(stderr, …)` goes, and what `std`'s own stderr writes to
    // (`_dup2` moves `STD_ERROR_HANDLE` along with descriptor 2). The
    // `libc` crate doesn't declare these for MSVC targets.
    extern "C" {
        fn _dup(fd: c_int) -> c_int;
        fn _dup2(from: c_int, to: c_int) -> c_int;
        fn _close(fd: c_int) -> c_int;
        fn _get_osfhandle(fd: c_int) -> isize;
        fn _open_osfhandle(handle: isize, flags: c_int) -> c_int;
    }

    pub fn dup(fd: Fd) -> Fd {
        // SAFETY: a dup of a descriptor we were given; the new one is ours.
        unsafe { _dup(fd) }
    }

    pub fn dup2(from: Fd, to: Fd) -> Fd {
        // SAFETY: both are descriptors we own or the well-known stderr.
        // `_dup2` gives `to` a handle of its own, so the two close apart.
        unsafe { _dup2(from, to) }
    }

    pub fn close(fd: Fd) {
        // SAFETY: a descriptor we own and don't use again.
        unsafe { _close(fd) };
    }

    /// Wraps `fd`, which the returned file then owns.
    ///
    /// A `File` is a handle rather than a descriptor here, so what is
    /// handed over is the handle behind `fd` and the descriptor slot is
    /// left behind: closing it as well would close the handle twice.
    /// Each call costs one slot, and this is called a few times a run.
    pub fn file_from_fd(fd: Fd) -> std::fs::File {
        use std::os::windows::io::{FromRawHandle, RawHandle};
        // SAFETY: `fd` is ours, and its handle's only claim passes to
        // the file; the slot is never read or closed again.
        unsafe { std::fs::File::from_raw_handle(_get_osfhandle(fd) as RawHandle) }
    }

    /// A descriptor that writes into `file`, which the caller must not
    /// close: it shares the file's handle, and is left open so that the
    /// handle keeps exactly one owner — `file` itself.
    pub fn fd_for_writes(file: &std::fs::File) -> Fd {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: a live handle from a file we were lent, wrapped in a
        // descriptor we hand to `_dup2` and then abandon.
        unsafe { _open_osfhandle(file.as_raw_handle() as isize, O_BINARY) }
    }
}

#[cfg(unix)]
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

#[cfg(unix)]
fn is_sherpa_resampler_banner(line: &str) -> bool {
    line.contains("sherpa-onnx") && line.contains("Creating a resampler")
}

#[cfg(unix)]
fn starts_with_indent(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

#[cfg(unix)]
fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

#[cfg(test)]
mod tests {
    /// The descriptor swap itself, on whichever platform this is built
    /// for: what goes to stderr while the guard lives lands in the
    /// file, the terminal's own stderr is kept, and dropping the guard
    /// hands stderr back.
    #[test]
    fn redirect_to_sends_stderr_to_the_file() {
        use std::io::Write as _;

        let path = std::env::temp_dir().join("vox_transcribe-stderr-test.log");
        let _ = std::fs::remove_file(&path);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("open the log");

        {
            let _guard = super::redirect_to(&file).expect("redirect stderr");
            let mut err = std::io::stderr();
            err.write_all(b"into the log\n").expect("write to stderr");
            err.flush().expect("flush stderr");
        }
        // Whatever the terminal's stderr was, it was saved on the way.
        assert!(super::console().is_some(), "the original stderr was lost");

        let logged = std::fs::read_to_string(&path).expect("read the log");
        assert!(
            logged.contains("into the log"),
            "stderr went somewhere else: {logged:?}"
        );
        let _ = std::fs::remove_file(&path);
    }
}

//! Headless progress on stderr. When both stdout and stderr are a
//! terminal: a bar redrawn in place (and a spinner for work with no
//! measurable progress). When either is piped or redirected: one line per
//! 10%, which reads fine in a log or next to piped output.

use std::io::{IsTerminal as _, Write};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const REDRAW: Duration = Duration::from_millis(100);
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Where progress and status go: the terminal's own stderr, bypassing
/// the sherpa log filter (which forwards whole lines only) and a TUI's
/// redirect of fd 2 to the log file.
struct Console {
    out: Box<dyn Write + Send>,
    /// Draw bars and spinners: stderr is a terminal and stdout isn't
    /// being piped or redirected.
    tty: bool,
}

fn console() -> &'static Mutex<Console> {
    static CONSOLE: OnceLock<Mutex<Console>> = OnceLock::new();
    CONSOLE.get_or_init(|| {
        #[cfg(unix)]
        if let Some(f) = vox_transcribe::stderr::console() {
            let tty = f.is_terminal() && std::io::stdout().is_terminal();
            return Mutex::new(Console {
                out: Box::new(f),
                tty,
            });
        }
        Mutex::new(Console {
            tty: std::io::stderr().is_terminal() && std::io::stdout().is_terminal(),
            out: Box::new(std::io::stderr()),
        })
    })
}

fn is_tty() -> bool {
    console().lock().map(|c| c.tty).unwrap_or(false)
}

/// Write `s` to the console as is (no newline added).
fn put(s: &str) {
    if let Ok(mut c) = console().lock() {
        let _ = c.out.write_all(s.as_bytes());
        let _ = c.out.flush();
    }
}

/// Print a status line, first erasing any bar or spinner on a terminal.
pub fn say(msg: &str) {
    if let Ok(mut c) = console().lock() {
        let erase = if c.tty { "\r\x1b[2K" } else { "" };
        let _ = writeln!(c.out, "{erase}{msg}");
        let _ = c.out.flush();
    }
}

/// `eprintln!` for status lines, through [`say`].
#[macro_export]
macro_rules! say {
    ($($t:tt)*) => { $crate::progress::say(&format!($($t)*)) };
}

fn clock(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn term_width() -> usize {
    ratatui::crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(80)
}

/// Erase the current line (a bar or spinner) on a terminal.
pub fn clear_line() {
    if is_tty() {
        put("\r\x1b[2K");
    }
}

/// How much of a file input has been transcribed (pass 2 done).
pub struct Progress {
    len_ms: Option<u64>,
    done_ms: u64,
    tty: bool,
    started: Instant,
    last_draw: Option<Instant>,
    next_tenth: u64,
    shown: bool,
}

impl Progress {
    pub fn new(len_ms: Option<u64>) -> Self {
        Self {
            len_ms: len_ms.filter(|&l| l > 0),
            done_ms: 0,
            tty: is_tty(),
            started: Instant::now(),
            last_draw: None,
            next_tenth: 1,
            shown: false,
        }
    }

    /// Note the finalized clips of `p`.
    pub fn update(&mut self, p: &vox_transcribe::Paragraph) {
        let Some(len) = self.len_ms else { return };
        let done = p
            .clips
            .iter()
            .filter(|c| !c.is_partial())
            .map(|c| c.end_ms())
            .max()
            .unwrap_or(0);
        self.done_ms = self.done_ms.max(done.min(len));
        if !self.tty {
            let tenths = (self.done_ms * 10 / len).min(9);
            if tenths >= self.next_tenth {
                say(&format!("  {tenths}0%"));
                self.next_tenth = tenths + 1;
            }
        }
        self.tick();
    }

    /// Redraw the bar if it is due.
    pub fn tick(&mut self) {
        let Some(len) = self.len_ms.filter(|_| self.tty) else {
            return;
        };
        if self.last_draw.is_some_and(|t| t.elapsed() < REDRAW) {
            return;
        }
        self.last_draw = Some(Instant::now());
        let frac = self.done_ms as f64 / len as f64;
        let elapsed = self.started.elapsed();
        // ETA once there is enough to extrapolate from.
        let eta = (frac > 0.02 && elapsed > Duration::from_secs(2)).then(|| {
            let total = elapsed.as_secs_f64() / frac;
            ((total - elapsed.as_secs_f64()).max(0.0) * 1000.0) as u64
        });
        let spin = SPINNER[(elapsed.as_millis() / 100) as usize % SPINNER.len()];
        let tail = format!(
            " {:>3}%  {} / {}{}",
            (frac * 100.0).floor() as u32,
            clock(self.done_ms),
            clock(len),
            eta.map(|e| format!("  ~{} left", clock(e)))
                .unwrap_or_default()
        );
        let head = format!("{spin} transcribing ");
        let room = term_width().saturating_sub(head.chars().count() + tail.chars().count() + 1);
        let bar_w = room.min(40);
        let filled = ((bar_w as f64) * frac).round() as usize;
        put(&format!(
            "\r\x1b[2K{head}{}{}{tail}",
            "━".repeat(filled),
            "─".repeat(bar_w - filled.min(bar_w))
        ));
        self.shown = true;
    }

    /// Erase the bar, before printing anything else.
    pub fn clear(&mut self) {
        if self.shown {
            clear_line();
            self.last_draw = None;
        }
    }
}

/// Run `f` while a spinner with the elapsed time and `label` turns on
/// stderr (a terminal only; otherwise `label` is printed once).
pub fn spin_while<T>(label: &str, f: impl FnOnce() -> T) -> T {
    if !is_tty() {
        say(label);
        return f();
    }
    let started = Instant::now();
    let stop = std::sync::atomic::AtomicBool::new(false);
    let stop = &stop;
    std::thread::scope(|sc| {
        sc.spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let e = started.elapsed();
                let spin = SPINNER[(e.as_millis() / 100) as usize % SPINNER.len()];
                put(&format!(
                    "\r\x1b[2K{spin} {label} {}",
                    clock(e.as_millis() as u64)
                ));
                std::thread::sleep(REDRAW);
            }
            clear_line();
        });
        let out = f();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        out
    })
}

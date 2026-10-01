//! Copy text to the system clipboard: the platform tool when present,
//! otherwise the OSC 52 terminal escape (works over ssh in terminals
//! that support it, e.g. iTerm2, kitty, WezTerm, Alacritty).

use std::io::Write as _;
use std::process::{Command, Stdio};

/// Returns the method that was used.
pub fn copy(text: &str) -> std::io::Result<&'static str> {
    let tools: &[(&str, &[&str], bool)] = &[
        ("pbcopy", &[], cfg!(target_os = "macos")),
        (
            "wl-copy",
            &[],
            std::env::var_os("WAYLAND_DISPLAY").is_some(),
        ),
        (
            "xclip",
            &["-selection", "clipboard"],
            std::env::var_os("DISPLAY").is_some(),
        ),
    ];
    for &(cmd, args, applicable) in tools {
        if applicable && pipe_to(cmd, args, text).is_ok() {
            return Ok(cmd);
        }
    }
    let mut out = std::io::stdout();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()?;
    Ok("OSC 52")
}

fn pipe_to(cmd: &str, args: &[&str], text: &str) -> std::io::Result<()> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(text.as_bytes())?;
    if child.wait()?.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("{cmd} failed")))
    }
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
    }
}

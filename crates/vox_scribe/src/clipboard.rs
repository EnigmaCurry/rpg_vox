//! Copy text to the system clipboard: Win32 on Windows, the platform
//! tool when present, otherwise the OSC 52 terminal escape (works over
//! ssh in terminals that support it, e.g. iTerm2, kitty, WezTerm,
//! Alacritty).

use std::io::Write as _;
use std::process::{Command, Stdio};

/// Returns the method that was used.
pub fn copy(text: &str) -> std::io::Result<&'static str> {
    #[cfg(target_os = "windows")]
    if win32(text).is_ok() {
        return Ok("Win32");
    }
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

/// The Windows clipboard, through Win32. `clip.exe` would be the
/// equivalent of the tools above, but it reads its input in the
/// console's code page and so mangles everything that isn't ASCII —
/// accents, dashes, any language but English — which `--dictate` exists
/// to produce.
#[cfg(target_os = "windows")]
fn win32(text: &str) -> std::io::Result<()> {
    use std::io::Error;
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    // CF_UNICODETEXT is NUL-terminated UTF-16.
    let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: every call's result is checked before the next one uses
    // it. The moveable block is ours until `SetClipboardData` takes it,
    // and is freed on each path that doesn't get that far; after that
    // the clipboard owns it and must not be freed here.
    unsafe {
        let block = GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of_val(&utf16[..]));
        if block.is_null() {
            return Err(Error::last_os_error());
        }
        // Freeing the block and returning what went wrong.
        let failed = |e: Error| {
            GlobalFree(block);
            Err(e)
        };
        let locked = GlobalLock(block);
        if locked.is_null() {
            return failed(Error::last_os_error());
        }
        std::ptr::copy_nonoverlapping(utf16.as_ptr(), locked.cast::<u16>(), utf16.len());
        // Zero here is the lock count reaching zero, not a failure.
        GlobalUnlock(block);
        // No window of our own owns the clipboard; the console is fine.
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return failed(Error::last_os_error());
        }
        if EmptyClipboard() == 0 {
            let e = Error::last_os_error();
            CloseClipboard();
            return failed(e);
        }
        if SetClipboardData(CF_UNICODETEXT as u32, block).is_null() {
            let e = Error::last_os_error();
            CloseClipboard();
            return failed(e);
        }
        if CloseClipboard() == 0 {
            return Err(Error::last_os_error());
        }
    }
    Ok(())
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

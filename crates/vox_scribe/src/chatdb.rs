//! The `--chat` database lives in the `vox_chat` crate (shared with
//! `agent`); this adds `scribe chat-echo`, a stand-in responder.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
pub use vox_chat::{path_for, Db, Kind, Role, Row};

/// `scribe chat-echo NAME`: a stand-in responder. Each thing the user
/// says is read back a sentence per row, then, after a pause during
/// which scribe waits (`more`), a second utterance. Stops a reply as
/// soon as the user interrupts it or says something new.
pub fn echo(path: &Path) -> Result<()> {
    let db = Db::open(path)?;
    let mut last = db.last_id()?;
    println!("echoing replies in {} (ctrl+c to stop)", path.display());
    loop {
        let rows = db.since(last)?;
        let Some(say) = rows
            .iter()
            .rev()
            .find(|r| r.role == Role::User && r.kind == Kind::Say)
            .cloned()
        else {
            if let Some(r) = rows.last() {
                last = r.id;
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        // Only the newest thing said gets a reply.
        last = say.id;
        println!("user #{}: {}", say.id, say.text);
        let words = say.text.split_whitespace().count();
        let mut parts: Vec<(String, u64)> =
            crate::speak::sentences(&format!("You said: {}", say.text))
                .into_iter()
                .map(|s| (s, 300))
                .collect();
        // A second utterance after a pause, to show `more`.
        parts.push((
            format!(
                "That was {words} word{}. What else?",
                if words == 1 { "" } else { "s" }
            ),
            1500,
        ));
        let n = parts.len();
        for (i, (text, delay)) in parts.into_iter().enumerate() {
            std::thread::sleep(Duration::from_millis(delay));
            let cut = db.since(say.id)?.into_iter().any(|r| r.role == Role::User);
            if cut {
                println!("  interrupted");
                break;
            }
            db.reply(say.id, &text, i + 1 < n)?;
            println!("  reply: {text}");
        }
    }
}

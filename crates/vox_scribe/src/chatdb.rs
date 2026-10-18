//! The `--chat` database lives in the `vox_chat` crate (shared with
//! `agent`); this adds `scribe chat-echo`, a stand-in responder.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
pub use vox_chat::{path_for, Conversation, Db, Role, Turn, RESPONDER_STALE};

/// `scribe chat-echo NAME`: a stand-in responder. Each thing the user
/// says is read back a sentence at a time, then, after a pause during
/// which scribe waits, a second utterance. Stops a reply as soon as the
/// user talks over it or says something new.
pub fn echo(path: &Path) -> Result<()> {
    let db = Db::open(path)?;
    // So `scribe --chat` sees a responder is listening.
    vox_chat::beat(path, &format!("chat-echo {}", std::process::id()))?;
    println!("echoing replies in {} (ctrl+c to stop)", path.display());
    // What we last took up; see `agent`'s loop for why the row order
    // alone won't do.
    let mut answered = match db.unanswered()? {
        Some(_) => 0,
        None => db.newest_user()?.map_or(0, |t| t.id),
    };
    loop {
        let Some(say) = db.newest_user()?.filter(|t| t.done && t.id != answered) else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        answered = say.id;
        println!("user #{}: {}", say.id, say.text);
        // An empty turn is what scribe shows as thinking.
        let reply = db.start(say.conversation, Role::Assistant, "", false)?;
        let words = say.text.split_whitespace().count();
        let mut parts: Vec<(String, u64)> =
            crate::speak::sentences(&format!("You said: {}", say.text))
                .into_iter()
                .map(|s| (s, 300))
                .collect();
        // A second utterance after a pause, to show scribe waiting.
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
            if db.said_since(say.rev)? {
                println!("  interrupted");
                db.extend(reply, "", true)?;
                break;
            }
            let last = i + 1 == n;
            db.extend(reply, &format!("{}{text}", if i > 0 { " " } else { "" }), last)?;
            println!("  reply: {text}");
        }
    }
}

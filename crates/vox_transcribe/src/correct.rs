//! Pass 4: LLM correction of a settled paragraph.
//!
//! The model proposes `{from, to}` substring edits instead of rewriting
//! the paragraph, and [`apply_edits`] rejects anything that would change
//! too much, so pass 4 stays a proofreader rather than an editor.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    pub from: String,
    pub to: String,
}

/// Something that proposes corrections, typically an LLM.
pub trait Corrector: Send + Sync {
    /// `context` is the text of the preceding paragraphs, oldest first.
    fn correct(&self, text: &str, context: &[String]) -> anyhow::Result<Vec<Edit>>;
}

/// Most of a paragraph's words that pass 4 may change.
pub const MAX_CHANGED_FRACTION: f64 = 0.4;

/// Apply `edits` in order, each to the first occurrence of its `from`
/// (edits whose `from` isn't present are skipped). Returns the new text
/// and how many edits applied, or an error if the result changes more
/// than [`MAX_CHANGED_FRACTION`] of the words.
pub fn apply_edits(text: &str, edits: &[Edit]) -> Result<(String, usize), String> {
    let mut out = text.to_string();
    let mut applied = 0;
    for e in edits {
        if e.from.is_empty() || e.from == e.to {
            continue;
        }
        if let Some(pos) = out.find(&e.from) {
            out.replace_range(pos..pos + e.from.len(), &e.to);
            applied += 1;
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let before: Vec<&str> = text.split_whitespace().collect();
    let after: Vec<&str> = out.split_whitespace().collect();
    let kept = lcs_len(&before, &after);
    let changed = before.len().max(after.len()) - kept;
    let total = before.len().max(1);
    if before.len() >= 5 && changed as f64 / total as f64 > MAX_CHANGED_FRACTION {
        return Err(format!("edits changed {changed} of {total} words"));
    }
    if after.is_empty() && !before.is_empty() {
        return Err("edits removed everything".into());
    }
    Ok((out, applied))
}

/// Indices of words in `after` that are not part of the longest common
/// subsequence with `before` (i.e. words pass 4 inserted or changed).
pub fn changed_words(before: &[&str], after: &[&str]) -> Vec<bool> {
    let t = lcs_table(before, after);
    let (mut i, mut j) = (before.len(), after.len());
    let mut changed = vec![true; after.len()];
    while i > 0 && j > 0 {
        if before[i - 1] == after[j - 1] {
            changed[j - 1] = false;
            i -= 1;
            j -= 1;
        } else if t[i - 1][j] >= t[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    changed
}

fn lcs_table(a: &[&str], b: &[&str]) -> Vec<Vec<u32>> {
    let mut t = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            t[i][j] = if a[i - 1] == b[j - 1] {
                t[i - 1][j - 1] + 1
            } else {
                t[i - 1][j].max(t[i][j - 1])
            };
        }
    }
    t
}

fn lcs_len(a: &[&str], b: &[&str]) -> usize {
    lcs_table(a, b)[a.len()][b.len()] as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(from: &str, to: &str) -> Edit {
        Edit {
            from: from.into(),
            to: to.into(),
        }
    }

    #[test]
    fn applies_small_fixes() {
        let text = "It's Wednesday at 11,42 PM, which is One,42 in the morning in. Graftton.";
        let (out, n) = apply_edits(
            text,
            &[
                e("11,42", "11:42"),
                e("One,42", "1:42"),
                e("in. Graftton", "in Grafton"),
                e("missing", "x"),
            ],
        )
        .unwrap();
        assert_eq!(n, 3);
        assert_eq!(
            out,
            "It's Wednesday at 11:42 PM, which is 1:42 in the morning in Grafton."
        );
    }

    #[test]
    fn rejects_rewrites() {
        let text = "one two three four five six seven eight";
        let err = apply_edits(
            text,
            &[e("two three four five six", "something else entirely")],
        );
        assert!(err.is_err());
    }

    #[test]
    fn marks_changed_words() {
        let before = ["at", "11,42", "PM"];
        let after = ["at", "11:42", "PM"];
        assert_eq!(changed_words(&before, &after), vec![false, true, false]);
    }
}

//! Spoken digit strings to numerals.
//!
//! Recognizers spell out digits read one at a time ("zero seven eight
//! four zero dash one five zero three"). [`spoken_digits`] rewrites each
//! run of spelled-out digits as numerals, with the spoken separators
//! "dash" (`-`), "dot" / "point" (`.`) and "slash" (`/`) between them:
//! `07840-1503`, `192.168.1.1`.
//!
//! Only runs of two or more digits, or digits joined by a separator,
//! are touched, so prose like "one of them" or "the point is" stays as
//! it is.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    /// A spelled-out digit.
    Digit(char),
    /// A numeral the recognizer already wrote as digits.
    Numeral,
    Sep(char),
}

fn digit(word: &str) -> Option<char> {
    Some(match word {
        "zero" => '0',
        "one" => '1',
        "two" => '2',
        "three" => '3',
        "four" => '4',
        "five" => '5',
        "six" => '6',
        "seven" => '7',
        "eight" => '8',
        "nine" => '9',
        _ => return None,
    })
}

fn separator(word: &str) -> Option<char> {
    Some(match word {
        "dash" | "-" => '-',
        "dot" | "point" => '.',
        "slash" | "/" => '/',
        _ => return None,
    })
}

/// A whitespace token split into leading punctuation, the word, and
/// trailing punctuation.
struct Token<'a> {
    lead: &'a str,
    core: &'a str,
    trail: &'a str,
    item: Option<Item>,
}

fn token(raw: &str) -> Token<'_> {
    // A bare "-" or "/" is a separator, not punctuation around nothing.
    if let Some(sep) = separator(raw).filter(|_| !raw.chars().any(char::is_alphanumeric)) {
        return Token {
            lead: "",
            core: raw,
            trail: "",
            item: Some(Item::Sep(sep)),
        };
    }
    let start = raw.find(char::is_alphanumeric).unwrap_or(raw.len());
    let end = raw
        .rfind(char::is_alphanumeric)
        .map(|i| i + raw[i..].chars().next().map_or(1, char::len_utf8))
        .unwrap_or(start);
    let core = &raw[start..end];
    let lower = core.to_lowercase();
    let item = if !core.is_empty() && core.chars().all(|c| c.is_ascii_digit()) {
        Some(Item::Numeral)
    } else if let Some(d) = digit(&lower) {
        Some(Item::Digit(d))
    } else {
        separator(&lower).map(Item::Sep)
    };
    Token {
        lead: &raw[..start],
        core,
        trail: &raw[end..],
        item,
    }
}

/// Rewrite spoken digit runs in `text` as numerals (see the module docs).
/// Whitespace between words is normalized to single spaces wherever a
/// run is rewritten; text with no run comes back unchanged.
pub fn spoken_digits(text: &str) -> String {
    let raw: Vec<&str> = text.split_whitespace().collect();
    let tokens: Vec<Token> = raw.iter().map(|r| token(r)).collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut changed = false;
    let mut i = 0;
    while i < tokens.len() {
        let end = run_end(&tokens, i);
        if end > i && worth_rewriting(&tokens[i..end]) {
            let mut s = tokens[i].lead.to_string();
            for t in &tokens[i..end] {
                match t.item {
                    Some(Item::Digit(d) | Item::Sep(d)) => s.push(d),
                    Some(Item::Numeral) => s.push_str(t.core),
                    None => unreachable!(),
                }
            }
            s.push_str(tokens[end - 1].trail);
            out.push(s);
            changed = true;
            i = end;
        } else {
            out.push(raw[i].to_string());
            i += 1;
        }
    }
    if changed {
        out.join(" ")
    } else {
        text.to_string()
    }
}

/// End (exclusive) of the digit run starting at `start`, or `start` when
/// none starts there. A run starts and ends on a digit, has at most one
/// separator between two digits, and stops at punctuation other than a
/// comma (which recognizers sprinkle between spelled-out digits).
fn run_end(tokens: &[Token], start: usize) -> usize {
    let is_digit = |t: &Token| matches!(t.item, Some(Item::Digit(_) | Item::Numeral));
    if !is_digit(&tokens[start]) {
        return start;
    }
    let mut end = start + 1;
    let open = |t: &Token| t.trail.is_empty() || t.trail == ",";
    while end < tokens.len() && open(&tokens[end - 1]) {
        let next = &tokens[end];
        if !next.lead.is_empty() {
            break;
        }
        if is_digit(next) {
            end += 1;
        } else if matches!(next.item, Some(Item::Sep(_)))
            && open(next)
            && tokens
                .get(end + 1)
                .is_some_and(|t| t.lead.is_empty() && is_digit(t))
        {
            end += 2;
        } else {
            break;
        }
    }
    end
}

/// A lone digit is prose ("one of them"); so is a string of numerals
/// the recognizer already wrote, which would only get glued together.
fn worth_rewriting(run: &[Token]) -> bool {
    let spelled = run.iter().any(|t| matches!(t.item, Some(Item::Digit(_))));
    let sep = run.iter().any(|t| matches!(t.item, Some(Item::Sep(_))));
    (spelled && run.len() >= 2) || sep
}

#[cfg(test)]
mod tests {
    use super::spoken_digits as d;

    #[test]
    fn zip_code() {
        assert_eq!(
            d("zero seven eight four zero dash one five zero three"),
            "07840-1503"
        );
    }

    #[test]
    fn ip_address() {
        assert_eq!(
            d("one nine two dot one six eight dot one dot one"),
            "192.168.1.1"
        );
        assert_eq!(d("ten dot zero point zero dot one"), "ten dot 0.0.1");
    }

    #[test]
    fn point_and_slash() {
        assert_eq!(d("version two point five"), "version 2.5");
        assert_eq!(d("on one slash two slash three"), "on 1/2/3");
    }

    #[test]
    fn in_a_sentence() {
        assert_eq!(
            d("My zip is zero seven eight four zero, thanks."),
            "My zip is 07840, thanks."
        );
        assert_eq!(
            d("Ping one nine two dot one six eight dot one dot one."),
            "Ping 192.168.1.1."
        );
        assert_eq!(d("Call Five five five, one two one two."), "Call 5551212.");
    }

    #[test]
    fn numerals_already_written() {
        assert_eq!(d("192 dot 168 dot 1 dot 1"), "192.168.1.1");
        assert_eq!(d("zero seven - one five"), "07-15");
        assert_eq!(d("I have 3 4 cats"), "I have 3 4 cats");
    }

    #[test]
    fn prose_untouched() {
        for s in [
            "One of them was there.",
            "The point is, two dogs.",
            "one dash",
            "dash one",
            "one dot dot two",
            "Slash and dash.",
            "",
        ] {
            assert_eq!(d(s), s);
        }
        // A sentence end stops a run.
        assert_eq!(d("I said one. Two more."), "I said one. Two more.");
    }
}

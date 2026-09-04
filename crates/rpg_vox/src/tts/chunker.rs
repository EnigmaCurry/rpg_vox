//! Split an utterance into short phrases at natural punctuation boundaries.
//!
//! Piper synthesizes a whole phrase in one shot, so time-to-first-audio is
//! dominated by phrase length. We aim for ~5-20 words per phrase, breaking at
//! `.?!,:;` when we've accumulated enough words, and force-breaking at the
//! upper bound if the LLM writes a run-on sentence.

const STRONG_BREAKS: &[char] = &['.', '?', '!'];
const WEAK_BREAKS: &[char] = &[',', ':', ';'];

pub struct Config {
    pub min_words: usize,
    pub max_words: usize,
    /// Word count at which a weak break (`,:;`) is allowed to close a chunk.
    /// `None` derives it as `min_words + (max_words - min_words) / 2` — a
    /// midpoint that biases toward sentence-end breaks. Set equal to
    /// `min_words` to make commas break aggressively (audible comma pauses at
    /// the cost of shorter chunks and more intonation resets).
    pub weak_after_words: Option<usize>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min_words: 5,
            max_words: 20,
            weak_after_words: None,
        }
    }
}

fn ends_with_break(word: &str, breaks: &[char]) -> bool {
    word.trim_end_matches(|c: char| c == '"' || c == '\'' || c == ')' || c == ']')
        .ends_with(breaks)
}

pub fn chunk(text: &str, cfg: &Config) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut words_in_cur = 0usize;
    // Threshold beyond which we accept a weak break (,:;) — otherwise we hold
    // out for a strong break (.?!) to keep phrases on sentence boundaries.
    let weak_after = cfg
        .weak_after_words
        .unwrap_or_else(|| cfg.min_words.saturating_add((cfg.max_words - cfg.min_words) / 2))
        .max(cfg.min_words)
        .min(cfg.max_words);

    for word in text.split_whitespace() {
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
        words_in_cur += 1;

        let strong = ends_with_break(word, STRONG_BREAKS);
        let weak = ends_with_break(word, WEAK_BREAKS);
        let hit_min = words_in_cur >= cfg.min_words;
        let hit_weak_threshold = words_in_cur >= weak_after;
        let hit_max = words_in_cur >= cfg.max_words;

        if hit_max || (hit_min && strong) || (hit_weak_threshold && weak) {
            out.push(std::mem::take(&mut cur));
            words_in_cur = 0;
        }
    }

    let tail = cur.trim().to_string();
    if !tail.is_empty() {
        if let Some(last) = out.last_mut() {
            // Fold a stubby tail into the previous phrase so we don't
            // synthesize "yeah." on its own.
            if tail.split_whitespace().count() < cfg.min_words {
                last.push(' ');
                last.push_str(&tail);
                return out;
            }
        }
        out.push(tail);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{Config, chunk};

    #[test]
    fn splits_at_sentence_end_when_long_enough() {
        let cfg = Config::default();
        // Two sentences, each past min_words, so we break at the strong stop
        // between them rather than emitting the whole thing as one phrase.
        let phrases = chunk(
            "Welcome to the ancient tavern, weary traveler. \
             The fire is warm and the ale is cold, so pull up a chair.",
            &cfg,
        );
        assert_eq!(
            phrases,
            vec![
                "Welcome to the ancient tavern, weary traveler.".to_string(),
                "The fire is warm and the ale is cold, so pull up a chair.".to_string(),
            ]
        );
    }

    #[test]
    fn short_input_stays_whole() {
        // Under min_words * 2 we don't want to fragment things pointlessly.
        let cfg = Config::default();
        let phrases = chunk("Hi there, friend.", &cfg);
        assert_eq!(phrases, vec!["Hi there, friend.".to_string()]);
    }

    #[test]
    fn force_breaks_on_runon() {
        let cfg = Config {
            min_words: 3,
            max_words: 5,
            weak_after_words: None,
        };
        let phrases = chunk("one two three four five six seven eight", &cfg);
        assert_eq!(phrases, vec!["one two three four five", "six seven eight"]);
    }

    #[test]
    fn folds_short_tail() {
        let cfg = Config::default();
        let phrases = chunk("A long enough opening sentence here. Yeah.", &cfg);
        assert_eq!(
            phrases,
            vec!["A long enough opening sentence here. Yeah.".to_string()]
        );
    }

    #[test]
    fn empty_in_empty_out() {
        assert!(chunk("   ", &Config::default()).is_empty());
    }
}

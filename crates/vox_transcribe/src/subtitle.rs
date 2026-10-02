//! SRT / WebVTT cues from word timings.

use crate::timing::Word;

#[derive(Debug, Clone)]
pub struct CueConfig {
    /// Longest line, in characters.
    pub max_line_chars: usize,
    /// Lines per cue.
    pub max_lines: usize,
    pub max_cue_ms: u64,
    /// A silence this long between words ends the cue.
    pub pause_ms: u64,
    /// Short cues are held on screen this long when there is room.
    pub min_cue_ms: u64,
}

impl Default for CueConfig {
    fn default() -> Self {
        Self {
            max_line_chars: 42,
            max_lines: 2,
            max_cue_ms: 6000,
            pause_ms: 700,
            min_cue_ms: 1000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    /// Already wrapped; lines separated by `\n`.
    pub text: String,
}

fn ends_sentence(w: &str) -> bool {
    w.ends_with(['.', '?', '!'])
}

fn ends_clause(w: &str) -> bool {
    w.ends_with([',', ';', ':', '.', '?', '!'])
}

/// Characters of `words` joined by single spaces.
fn text_len(words: &[Word]) -> usize {
    let n: usize = words.iter().map(|w| w.text.chars().count()).sum();
    n + words.len().saturating_sub(1)
}

/// Split `words` into cues. No cue extends past `limit_ms` (the end of
/// the audio they came from) or into the next cue.
pub fn cues(words: &[Word], limit_ms: u64, cfg: &CueConfig) -> Vec<Cue> {
    let max_chars = cfg.max_line_chars * cfg.max_lines;
    let mut groups: Vec<&[Word]> = Vec::new();
    let mut start = 0;
    let mut chars = 0;
    for (i, w) in words.iter().enumerate() {
        let len = w.text.chars().count();
        if i > start {
            let first = &words[start];
            let prev = &words[i - 1];
            let full = !fits(&words[start..=i], cfg);
            let long = w.end_ms.saturating_sub(first.start_ms) > cfg.max_cue_ms;
            let pause = w.start_ms.saturating_sub(prev.end_ms) >= cfg.pause_ms;
            // End on a sentence once the cue has some substance, rather
            // than carrying one word of the next sentence.
            let sentence = ends_sentence(&prev.text) && chars >= max_chars / 3;
            if pause || sentence {
                groups.push(&words[start..i]);
                start = i;
                chars = 0;
            } else if full || long {
                // Overflowing: cut after the last clause break in the
                // last two thirds rather than mid-clause, so the next cue
                // doesn't open on the clause's last word or two.
                let cut = (start + 1..i)
                    .rev()
                    .take_while(|&k| text_len(&words[start..k]) * 3 >= chars)
                    .find(|&k| ends_clause(&words[k - 1].text))
                    .unwrap_or(i);
                groups.push(&words[start..cut]);
                start = cut;
                chars = text_len(&words[start..i]);
            }
        }
        chars += if chars == 0 { len } else { len + 1 };
    }
    if start < words.len() {
        groups.push(&words[start..]);
    }

    let mut out: Vec<Cue> = Vec::with_capacity(groups.len());
    for (gi, g) in groups.iter().enumerate() {
        let start_ms = g[0].start_ms;
        let next_start = groups
            .get(gi + 1)
            .map(|n| n[0].start_ms)
            .unwrap_or(limit_ms.max(start_ms));
        let spoken_end = g.iter().map(|w| w.end_ms).max().unwrap_or(start_ms);
        let end_ms = spoken_end
            .max(start_ms + cfg.min_cue_ms)
            .min(next_start.max(spoken_end));
        let text: Vec<&str> = g.iter().map(|w| w.text.as_str()).collect();
        out.push(Cue {
            start_ms,
            end_ms: end_ms.max(start_ms + 1),
            text: wrap(&text, cfg.max_line_chars),
        });
    }
    out
}

/// Whether `words` wrap into the cue's lines without any running long.
fn fits(words: &[Word], cfg: &CueConfig) -> bool {
    let text: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    let wrapped = wrap(&text, cfg.max_line_chars);
    wrapped.lines().count() <= cfg.max_lines
        && wrapped.lines().all(|l| l.chars().count() <= cfg.max_line_chars)
}

/// Break into two lines at the word boundary nearest the middle when
/// one line is too long.
fn wrap(words: &[&str], max_line: usize) -> String {
    let one = words.join(" ");
    if one.chars().count() <= max_line || words.len() < 2 {
        return one;
    }
    let total = one.chars().count();
    let mut best = (usize::MAX, 1);
    let mut left = 0;
    for k in 1..words.len() {
        left += words[k - 1].chars().count() + usize::from(k > 1);
        let right = total - left - 1;
        let score = left.max(right);
        if score < best.0 {
            best = (score, k);
        }
    }
    format!("{}\n{}", words[..best.1].join(" "), words[best.1..].join(" "))
}

fn clock(ms: u64, sep: char) -> String {
    format!(
        "{:02}:{:02}:{:02}{sep}{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// One SRT block, numbered `index` (from 1), with its trailing blank line.
pub fn srt(index: usize, c: &Cue) -> String {
    format!(
        "{index}\n{} --> {}\n{}\n\n",
        clock(c.start_ms, ','),
        clock(c.end_ms, ','),
        c.text
    )
}

/// Header that starts a WebVTT file.
pub const VTT_HEADER: &str = "WEBVTT\n\n";

/// One WebVTT cue with its trailing blank line.
pub fn vtt(c: &Cue) -> String {
    format!(
        "{} --> {}\n{}\n\n",
        clock(c.start_ms, '.'),
        clock(c.end_ms, '.'),
        c.text
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(spec: &[(&str, u64, u64)]) -> Vec<Word> {
        spec.iter()
            .map(|&(t, s, e)| Word {
                text: t.into(),
                start_ms: s,
                end_ms: e,
            })
            .collect()
    }

    #[test]
    fn breaks_on_pauses_and_holds_short_cues() {
        let w = words(&[("Hi", 0, 200), ("there", 250, 500), ("Bye", 3000, 3300)]);
        let c = cues(&w, 4000, &CueConfig::default());
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].start_ms, c[0].end_ms), (0, 1000));
        assert_eq!(c[0].text, "Hi there");
        assert_eq!((c[1].start_ms, c[1].end_ms), (3000, 4000));
    }

    #[test]
    fn wraps_long_cues_into_two_lines() {
        let spec: Vec<(String, u64, u64)> = (0..14)
            .map(|i| (format!("word{i}"), i * 200, i * 200 + 150))
            .collect();
        let spec: Vec<(&str, u64, u64)> =
            spec.iter().map(|(t, s, e)| (t.as_str(), *s, *e)).collect();
        let c = cues(&words(&spec), 10_000, &CueConfig::default());
        assert!(c.len() >= 2);
        for cue in &c {
            assert!(cue.text.lines().count() <= 2);
            assert!(cue.text.lines().all(|l| l.chars().count() <= 42), "{cue:?}");
        }
        // Cues never overlap.
        assert!(c.windows(2).all(|p| p[0].end_ms <= p[1].start_ms));
    }

    #[test]
    fn overflow_cuts_at_a_clause_break() {
        let text = "Then the party fled north toward Grafton, at 11:42 p.m., hoping the bridge was still standing.";
        let spec: Vec<(&str, u64, u64)> = text
            .split(' ')
            .enumerate()
            .map(|(i, t)| (t, i as u64 * 300, i as u64 * 300 + 250))
            .collect();
        let c = cues(&words(&spec), 10_000, &CueConfig::default());
        let texts: Vec<String> = c.iter().map(|c| c.text.replace('\n', " ")).collect();
        assert_eq!(
            texts,
            vec![
                "Then the party fled north toward Grafton, at 11:42 p.m.,",
                "hoping the bridge was still standing."
            ]
        );
    }

    #[test]
    fn formats_clocks() {
        let c = Cue {
            start_ms: 3_723_004,
            end_ms: 3_724_500,
            text: "x".into(),
        };
        assert_eq!(srt(1, &c), "1\n01:02:03,004 --> 01:02:04,500\nx\n\n");
        assert_eq!(vtt(&c), "01:02:03.004 --> 01:02:04.500\nx\n\n");
    }
}

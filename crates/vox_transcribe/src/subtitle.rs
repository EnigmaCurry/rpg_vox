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
    /// The cue's words with their own times, for karaoke output.
    pub words: Vec<Word>,
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
            words: g.to_vec(),
        });
    }
    out
}

/// Whether `words` wrap into the cue's lines without any running long.
fn fits(words: &[Word], cfg: &CueConfig) -> bool {
    let text: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    let wrapped = wrap(&text, cfg.max_line_chars);
    wrapped.lines().count() <= cfg.max_lines
        && wrapped
            .lines()
            .all(|l| l.chars().count() <= cfg.max_line_chars)
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
    format!(
        "{}\n{}",
        words[..best.1].join(" "),
        words[best.1..].join(" ")
    )
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

/// Header that starts an ASS file. Karaoke turns each word from the
/// secondary colour (white, not yet spoken) to the primary colour
/// (yellow) as it is spoken. Diarized lines use the style named after
/// their speaker's slot in [`SPEAKER_STYLES`] instead, which only changes
/// the spoken colour.
pub const ASS_HEADER: &str = "[Script Info]
ScriptType: v4.00+
PlayResX: 1280
PlayResY: 720
WrapStyle: 2
ScaledBorderAndShadow: yes

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,52,&H0000FFFF,&H00FFFFFF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,3,1,2,60,60,50,1
Style: S1,Arial,52,&H00FF87FF,&H00FFFFFF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,3,1,2,60,60,50,1
Style: S2,Arial,52,&H0087D787,&H00FFFFFF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,3,1,2,60,60,50,1
Style: S3,Arial,52,&H005FAFFF,&H00FFFFFF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,3,1,2,60,60,50,1
Style: S4,Arial,52,&H00FF87AF,&H00FFFFFF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,3,1,2,60,60,50,1
Style: S5,Arial,52,&H00D7D75F,&H00FFFFFF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,3,1,2,60,60,50,1
Style: S6,Arial,52,&H0087D7FF,&H00FFFFFF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,3,1,2,60,60,50,1
Style: S7,Arial,52,&H005F5FFF,&H00FFFFFF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,3,1,2,60,60,50,1
Style: S8,Arial,52,&H00FFD7AF,&H00FFFFFF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,3,1,2,60,60,50,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
";

fn ass_clock(ms: u64) -> String {
    let cs = (ms + 5) / 10;
    format!(
        "{}:{:02}:{:02}.{:02}",
        cs / 360_000,
        cs / 6000 % 60,
        cs / 100 % 60,
        cs % 100
    )
}

/// Number of per-speaker styles (`S1` … `S8`) in [`ASS_HEADER`]; later
/// speakers reuse them in turn.
pub const SPEAKER_STYLES: usize = 8;

/// One ASS dialogue line whose `\k` tags highlight each word over its
/// own time. Durations are taken from rounded absolute times, so
/// rounding never accumulates across a cue.
pub fn ass(c: &Cue) -> String {
    ass_for(c, None)
}

/// Like [`ass`], for a cue spoken by `speaker`: its label ("A", "B", …),
/// which picks the line's style (colour), and the name shown for it
/// ("Speaker A", or one the user gave), which goes in the Name field.
pub fn ass_for(c: &Cue, speaker: Option<(&str, &str)>) -> String {
    let cs = |ms: u64| (ms + 5) / 10;
    let first_line_words = c
        .text
        .lines()
        .next()
        .map(|l| l.split_whitespace().count())
        .unwrap_or(0);
    let mut out = String::new();
    let mut at = cs(c.start_ms);
    for (i, w) in c.words.iter().enumerate() {
        if i > 0 {
            out.push_str(if i == first_line_words { "\\N" } else { " " });
        }
        let start = cs(w.start_ms).max(at);
        if start > at {
            // Silence before the word: nothing lights up.
            out.push_str(&format!("{{\\k{}}}", start - at));
        }
        let end = cs(w.end_ms).max(start);
        out.push_str(&format!("{{\\k{}}}{}", end - start, ass_escape(&w.text)));
        at = end;
    }
    let (style, name) = match speaker.and_then(|(l, n)| crate::speaker::index(l).map(|i| (i, n))) {
        Some((i, n)) => (format!("S{}", i % SPEAKER_STYLES + 1), n.replace(',', " ")),
        None => ("Default".to_string(), String::new()),
    };
    format!(
        "Dialogue: 0,{},{},{style},{name},0,0,0,,{}\n",
        ass_clock(c.start_ms),
        ass_clock(c.end_ms),
        out
    )
}

fn ass_escape(s: &str) -> String {
    s.replace('{', "(").replace('}', ")").replace('\\', "/")
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
    fn karaoke_times_each_word() {
        let w = words(&[("Hi", 280, 500), ("there,", 500, 900), ("you", 1300, 1500)]);
        let mut c = cues(&w, 2000, &CueConfig::default()).remove(0);
        assert_eq!(
            ass(&c),
            "Dialogue: 0,0:00:00.28,0:00:01.50,Default,,0,0,0,,{\\k22}Hi {\\k40}there, {\\k40}{\\k20}you\n"
        );
        c.text = "Hi there,\nyou".into();
        assert!(ass(&c).contains("there,\\N{\\k40}"));
    }

    #[test]
    fn formats_clocks() {
        let c = Cue {
            start_ms: 3_723_004,
            end_ms: 3_724_500,
            text: "x".into(),
            words: Vec::new(),
        };
        assert_eq!(srt(1, &c), "1\n01:02:03,004 --> 01:02:04,500\nx\n\n");
        assert_eq!(vtt(&c), "01:02:03.004 --> 01:02:04.500\nx\n\n");
    }
}

//! A dictation's text as `--dictate` edits it: a cursor that sends new
//! speech to a gap between words, deleted words, and paragraph breaks.
//! The same model as Scribe for Android's dictations (`DictationDoc.kt`).
//! All of it is kept by time on the session's audio clock (ms), so it
//! holds when later passes rewrite the text. The engine's own paragraphs
//! run together; only breaks made here split them.
//!
//! Text from before (the file, or what was typed by hand, see
//! [`Doc::load`]) stands just before the clock's `origin`, so the same
//! cursor and cuts work on it.

use std::collections::HashSet;

use vox_transcribe::Paragraph;

/// One utterance as the engine sent it.
#[derive(Clone, Debug)]
pub struct KClip {
    pub partial: bool,
    pub start_ms: i64,
    pub words: Vec<String>,
    /// One per word when known (finished clips); a word without a time
    /// can't be deleted or have the cursor put beside it.
    pub times: Vec<(i64, i64)>,
}

/// The clips of `paras` in the order said, leaving out `skip` (clips
/// already folded into the text). Partial (pass-1) words go through
/// `partial` for display.
pub fn clips(
    paras: &[Paragraph],
    skip: &HashSet<String>,
    partial: impl Fn(&str) -> Vec<String>,
) -> Vec<KClip> {
    let mut out: Vec<KClip> = paras
        .iter()
        .flat_map(|p| &p.clips)
        .filter(|c| !skip.contains(&c.id))
        .map(|c| {
            let start_ms = c.start_ms as i64;
            if c.is_partial() {
                return KClip {
                    partial: true,
                    start_ms,
                    words: partial(&c.text),
                    times: Vec::new(),
                };
            }
            let words: Vec<String> = c.text.split_whitespace().map(str::to_string).collect();
            let timed = c.timed_words();
            let times = if timed.len() == words.len() {
                timed
                    .iter()
                    .map(|w| (w.start_ms as i64, w.end_ms as i64))
                    .collect()
            } else {
                Vec::new()
            };
            KClip {
                partial: false,
                start_ms,
                words,
                times,
            }
        })
        .collect();
    out.sort_by_key(|c| c.start_ms);
    out
}

/// A word of the text, a paragraph break, or a mark that holds where a
/// cursor move's speech starts (and shows nothing).
#[derive(Clone, Debug, PartialEq)]
pub struct Tok {
    pub text: String,
    /// Its span of audio, for heard words and the text from before.
    pub time: Option<(i64, i64)>,
    /// When it was said (its clip's start if untimed).
    pub ms: i64,
    pub partial: bool,
    /// The cursor move whose speech it is, 0 for the text from before.
    pub seg: usize,
    pub mark: bool,
    pub brk: bool,
    /// First word of its clip (utterance).
    pub head: bool,
}

impl Tok {
    fn mark(seg: usize) -> Self {
        Self {
            text: String::new(),
            time: None,
            ms: i64::MIN,
            partial: false,
            seg,
            mark: true,
            brk: false,
            head: false,
        }
    }

    /// Finished text that can be deleted or have the cursor put beside it.
    pub fn placed(&self) -> bool {
        self.time.is_some() || self.brk
    }

    /// Stands for the same word or break across rebuilds.
    pub fn key(&self) -> (i64, bool) {
        (self.ms, self.brk)
    }
}

/// A cursor move: speech from `from_ms` on goes after the last word of
/// move `seg`'s speech said by `after_ms`, or at its start.
#[derive(Clone, Debug)]
struct Insertion {
    seg: usize,
    after_ms: i64,
    from_ms: i64,
}

const BREAK: &str = "\n\n";

#[derive(Default)]
pub struct Doc {
    /// The engine's clips so far.
    pub clips: Vec<KClip>,
    /// Words from before, each at its time just before `origin`.
    base: Vec<(String, i64)>,
    /// Spans of audio (inclusive) whose words were deleted.
    cuts: Vec<(i64, i64)>,
    /// Paragraph breaks, each at a time on the clock.
    breaks: Vec<i64>,
    /// Cursor moves, in order.
    inserts: Vec<Insertion>,
    /// The latest time handed out by `stamp`.
    stamped: i64,
    /// Where the clock stood when the text was loaded: a clip said before
    /// it but heard only after counts as said then, at the cursor.
    origin: i64,
}

impl Doc {
    fn is_cut(&self, start: i64) -> bool {
        self.cuts.iter().any(|&(a, b)| (a..=b).contains(&start))
    }

    pub fn shown(&self, t: &Tok) -> bool {
        !t.mark && t.time.is_none_or(|(s, _)| !self.is_cut(s))
    }

    /// `now`, or just after the last time handed out if that's later: a
    /// cursor move or break made while stopped must still come after the
    /// one before.
    fn stamp(&mut self, now: i64) -> i64 {
        self.stamped = now.max(self.stamped.saturating_add(1));
        self.stamped
    }

    fn seg_of(&self, ms: i64) -> usize {
        self.inserts
            .iter()
            .rposition(|ins| ins.from_ms <= ms)
            .map_or(0, |i| i + 1)
    }

    /// The text in reading order, cursor moves applied and seams polished.
    pub fn build(&self) -> Vec<Tok> {
        self.polish(self.document())
    }

    /// All the words in reading order. Each cursor move's speech, in the
    /// order said, goes after the word it was placed after, found by time
    /// so later passes rewriting the text don't move it.
    fn document(&self) -> Vec<Tok> {
        let mut toks: Vec<Tok> = Vec::new();
        for (w, ms) in &self.base {
            toks.push(Tok {
                text: w.clone(),
                time: Some((*ms, ms + 1)),
                ms: *ms,
                partial: false,
                seg: self.seg_of(*ms),
                mark: false,
                brk: false,
                head: false,
            });
        }
        for c in &self.clips {
            for (i, w) in c.words.iter().enumerate() {
                let time = c.times.get(i).copied();
                let ms = time.map_or(c.start_ms, |t| t.0).max(self.origin);
                toks.push(Tok {
                    text: w.clone(),
                    time,
                    ms,
                    partial: c.partial,
                    seg: self.seg_of(ms),
                    mark: false,
                    brk: false,
                    head: i == 0,
                });
            }
        }
        for &ms in &self.breaks {
            toks.push(Tok {
                text: BREAK.into(),
                time: None,
                ms,
                partial: false,
                seg: self.seg_of(ms),
                mark: false,
                brk: true,
                head: false,
            });
        }
        // Stable: words of one clip keep their order.
        toks.sort_by_key(|t| t.ms);
        let mut by_seg: Vec<Vec<Tok>> = vec![Vec::new(); self.inserts.len() + 1];
        for t in toks {
            let s = t.seg;
            by_seg[s].push(t);
        }
        let mut doc = vec![Tok::mark(0)];
        doc.append(&mut by_seg[0]);
        for (i, ins) in self.inserts.iter().enumerate() {
            let at = match doc
                .iter()
                .rposition(|t| !t.mark && t.seg == ins.seg && t.ms <= ins.after_ms)
            {
                Some(last) => last + 1,
                None => {
                    doc.iter()
                        .position(|t| t.mark && t.seg == ins.seg)
                        .expect("every move has its mark")
                        + 1
                }
            };
            let mut run = vec![Tok::mark(i + 1)];
            run.append(&mut by_seg[i + 1]);
            doc.splice(at..at, run);
        }
        doc
    }

    /// Start over from `text` (a new line starts a paragraph), nothing
    /// said yet since `origin` on the clock, with the cursor before its
    /// `cursor`th word or break (or at the end).
    pub fn load(&mut self, text: &str, cursor: usize, origin: i64) {
        self.reset(items(text), cursor, origin);
    }

    fn reset(&mut self, items: Vec<String>, cursor: usize, origin: i64) {
        self.clips.clear();
        self.cuts.clear();
        self.breaks.clear();
        self.inserts.clear();
        self.stamped = i64::MIN;
        self.origin = origin;
        let n = items.len() as i64;
        // Two ms apart, ending before the origin.
        let times: Vec<i64> = (0..n).map(|i| origin - 2 * (n - i)).collect();
        self.base = items
            .iter()
            .zip(&times)
            .filter(|(w, _)| *w != BREAK)
            .map(|(w, &t)| (w.clone(), t))
            .collect();
        self.breaks = items
            .iter()
            .zip(&times)
            .filter(|(w, _)| *w == BREAK)
            .map(|(_, &t)| t)
            .collect();
        // New speech is a cursor move's even at the end, so it's fitted to
        // the sentence it lands in.
        let at = cursor.min(items.len());
        self.inserts.push(Insertion {
            seg: 0,
            after_ms: if at > 0 { times[at - 1] } else { i64::MIN },
            from_ms: origin,
        });
    }

    /// `doc` with the seams of cursor moves fixed up: the engine starts
    /// every utterance as a sentence, so speech put mid-sentence begins
    /// with a capital and may end with a full stop. Where the word before
    /// doesn't end a sentence, the first word is lowercased unless it
    /// looks like a name: "I", an acronym, inner capitals, or capitalized
    /// elsewhere mid-sentence. A full stop before a lowercase word of the
    /// old text is dropped. Speech put mid-sentence is a fragment, so the
    /// same goes for the utterances within it.
    fn polish(&self, doc: Vec<Tok>) -> Vec<Tok> {
        let mut out = doc.clone();
        let words: Vec<usize> = (0..doc.len()).filter(|&i| self.shown(&doc[i])).collect();
        let upper = |t: &Tok| t.text.chars().next().is_some_and(char::is_uppercase);
        let lower_first = |t: &Tok| t.text.chars().next().is_some_and(char::is_lowercase);
        let core = |w: &str| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
        // Words the engine capitalized mid-utterance after a lowercase
        // word, so likely names. Not after a capital: an utterance's first
        // word is capitalized as a sentence start and can drag the next
        // one along ("Lazy Brown").
        let mut names: HashSet<String> = HashSet::new();
        for k in 1..words.len() {
            let prev = &doc[words[k - 1]];
            let t = &doc[words[k]];
            if !t.brk
                && !t.partial
                && !t.head
                && t.seg == prev.seg
                && !prev.brk
                && lower_first(prev)
                && upper(t)
            {
                names.insert(core(&t.text));
            }
        }
        let name = |w: &str| {
            let c = core(w);
            c == "I"
                || c.starts_with("I'")
                || c.starts_with("I’")
                || c.chars().skip(1).any(char::is_uppercase)
                || names.contains(&c)
        };
        // Lowercase the utterance head words[k] and the run of capitals it
        // dragged along.
        let lower = |out: &mut Vec<Tok>, k: usize| {
            let seg = doc[words[k]].seg;
            for (m, &w) in words.iter().enumerate().skip(k) {
                let t = &doc[w];
                if m > k && (t.head || t.brk || t.partial || t.seg != seg) {
                    break;
                }
                if !upper(t) || name(&t.text) {
                    break;
                }
                let mut cs = out[w].text.chars();
                let first: String = cs
                    .next()
                    .map(|c| c.to_lowercase().collect())
                    .unwrap_or_default();
                out[w].text = first + cs.as_str();
            }
        };
        let drop_stop = |out: &mut Vec<Tok>, i: usize| {
            let last = &out[i].text;
            if last.ends_with('.') && !last.ends_with("..") {
                out[i].text.pop();
            }
        };
        let mut fragment: HashSet<usize> = HashSet::new();
        for k in 1..words.len() {
            let (i, j) = (words[k - 1], words[k]);
            let (prev, t) = (&doc[i], &doc[j]);
            if t.brk || prev.brk || t.partial || prev.partial {
                continue;
            }
            if t.seg == prev.seg {
                // An utterance within a fragment: carry the sentence on.
                if !t.head || !fragment.contains(&t.seg) {
                    continue;
                }
                drop_stop(&mut out, i);
                if !ends(&out[i]) {
                    lower(&mut out, k);
                }
                continue;
            }
            if !ends(prev) {
                fragment.insert(t.seg);
                lower(&mut out, k);
            }
            // out[i], not prev: the first word may have been lowercased already.
            if lower_first(t) {
                drop_stop(&mut out, i);
            }
        }
        out
    }

    /// Where in `doc` new speech goes: after the latest cursor move's words.
    pub fn cursor_at(&self, doc: &[Tok]) -> usize {
        let seg = self.inserts.len();
        doc.iter().rposition(|t| t.seg == seg).map_or(0, |i| i + 1)
    }

    /// The indices into `doc` of what is shown, in order (each break once
    /// and none first), and how many of them stand before the cursor.
    pub fn pieces(&self, doc: &[Tok]) -> (Vec<usize>, usize) {
        let cursor = self.cursor_at(doc);
        let mut out = Vec::new();
        let mut caret = None;
        let mut last_brk = true; // nor one first
        for (i, t) in doc.iter().enumerate() {
            if i == cursor {
                caret = Some(out.len());
            }
            if !self.shown(t) {
                continue;
            }
            // Two breaks can't touch, but a break said meanwhile could land
            // beside one; show it once.
            if t.brk && last_brk {
                continue;
            }
            last_brk = t.brk;
            out.push(i);
        }
        let caret = caret.unwrap_or(out.len());
        (out, caret)
    }

    /// The text as saved: no partial words, paragraphs a blank line apart.
    pub fn text(&self) -> String {
        self.text_and_caret(&self.build()).0
    }

    /// The text of `doc` as saved, and the cursor's char offset in it.
    pub fn text_and_caret(&self, doc: &[Tok]) -> (String, usize) {
        let (pieces, caret) = self.pieces(doc);
        let mut paras: Vec<Vec<&str>> = vec![Vec::new()];
        // (paragraph, words before the cursor in it)
        let mut at = (0, 0);
        for (n, &i) in pieces.iter().enumerate() {
            if n == caret {
                at = (paras.len() - 1, paras.last().map_or(0, Vec::len));
            }
            let t = &doc[i];
            if t.partial {
                continue;
            }
            if t.brk {
                paras.push(Vec::new());
            } else {
                paras.last_mut().expect("a paragraph").push(&t.text);
            }
        }
        if caret >= pieces.len() {
            at = (paras.len() - 1, paras.last().map_or(0, Vec::len));
        }
        let mut text = String::new();
        let mut offset = 0;
        for (p, words) in paras.iter().enumerate() {
            if words.is_empty() {
                if p == at.0 {
                    offset = text.chars().count();
                }
                continue;
            }
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            for (w, word) in words.iter().enumerate() {
                if p == at.0 && w == at.1 {
                    offset = text.chars().count();
                }
                if w > 0 {
                    text.push(' ');
                }
                text.push_str(word);
            }
            if p == at.0 && at.1 >= words.len() {
                offset = text.chars().count();
            }
        }
        (text, offset)
    }

    /// Send new speech to the gap before `doc[index]` (after it if
    /// `after`): `now` ends the utterance in progress and gives the time.
    /// False if the cursor is there already.
    pub fn move_cursor(
        &mut self,
        doc: &[Tok],
        index: usize,
        after: bool,
        now: impl FnOnce() -> i64,
    ) -> bool {
        let gap = if after { index + 1 } else { index };
        let cursor = self.cursor_at(doc);
        // Already there (only hidden words or marks between): nothing moves.
        let (a, b) = (gap.min(cursor), gap.max(cursor));
        if !doc[a..b].iter().any(|t| self.shown(t)) {
            return false;
        }
        // The last move had nothing said yet: take it back rather than
        // pile up moves (one per arrow key).
        let last = self.inserts.len();
        let mut prev = &doc[gap - 1];
        if !doc.iter().any(|t| !t.mark && t.seg == last) {
            if prev.mark && prev.seg == last {
                prev = &doc[gap - 2];
            }
            let (seg, after_ms) = (prev.seg, if prev.mark { i64::MIN } else { prev.ms });
            // Still ends an utterance just begun, so it follows the move.
            now();
            let ins = self.inserts.last_mut().expect("a move");
            ins.seg = seg;
            ins.after_ms = after_ms;
            return true;
        }
        let (seg, after_ms) = (prev.seg, if prev.mark { i64::MIN } else { prev.ms });
        let from_ms = self.stamp(now());
        self.inserts.push(Insertion {
            seg,
            after_ms,
            from_ms,
        });
        true
    }

    /// A new paragraph at the cursor. Never two in a row, nor one first:
    /// false beside a break already there or with nothing before it.
    pub fn paragraph_break(&mut self, doc: &[Tok], now: impl FnOnce() -> i64) -> bool {
        let cursor = self.cursor_at(doc);
        let before = doc[..cursor].iter().rev().find(|t| self.shown(t));
        let after = doc[cursor..].iter().find(|t| self.shown(t));
        if before.is_none_or(|t| t.brk) || after.is_some_and(|t| t.brk) {
            return false;
        }
        let ms = self.stamp(now());
        self.breaks.push(ms);
        true
    }

    /// Delete `picked`: words are cut, breaks removed.
    pub fn delete(&mut self, picked: &[&Tok]) {
        for t in picked {
            if t.brk {
                self.breaks.retain(|&b| b != t.ms);
            } else if let Some((s, e)) = t.time {
                self.cuts.push((s, s.max(e - 1)));
            }
        }
        // Breaks now side by side become one, so deleting it later
        // doesn't uncover another.
        let mut last_brk = true; // nor one first
        let shown: Vec<Tok> = self
            .document()
            .into_iter()
            .filter(|t| self.shown(t))
            .collect();
        for t in shown {
            if t.brk && last_brk {
                self.breaks.retain(|&b| b != t.ms);
            }
            last_brk = t.brk;
        }
    }

    /// Delete the sentence before the cursor: back to the end of the one
    /// before it or the paragraph's start. Right after a break, the break
    /// goes instead, joining the paragraphs. False if there's nothing.
    pub fn delete_sentence(&mut self, doc: &[Tok]) -> bool {
        let cursor = self.cursor_at(doc);
        let mut before = doc[..cursor]
            .iter()
            .rev()
            .filter(|t| self.shown(t) && t.placed());
        let Some(last) = before.next() else {
            return false;
        };
        if last.brk {
            self.delete(&[last]);
            return true;
        }
        let mut picked = vec![last];
        for t in before {
            if ends(t) {
                break;
            }
            picked.push(t);
        }
        self.delete(&picked);
        true
    }
}

/// `text`'s words, and a break between each paragraph and the next (a
/// new line starts one).
pub fn items(text: &str) -> Vec<String> {
    let mut items = Vec::new();
    for p in text.split('\n') {
        let words: Vec<&str> = p.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        if !items.is_empty() {
            items.push(BREAK.to_string());
        }
        items.extend(words.into_iter().map(str::to_string));
    }
    items
}

/// A saved file's text with one line per paragraph: paragraphs are
/// separated by blank lines, and the lines within one are joined, so a
/// file wrapped by hand reads back the same.
pub fn unwrap_file(text: &str) -> String {
    let mut paras: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                paras.push(cur.join(" "));
                cur.clear();
            }
        } else {
            cur.push(line.trim());
        }
    }
    if !cur.is_empty() {
        paras.push(cur.join(" "));
    }
    paras.join("\n")
}

/// Where the cursor goes back in the text after typing: before the word
/// or break `chars[..at]` leaves it before. A cursor within a word counts
/// as after it; one at a line's start as past the break before that line.
pub fn cursor_after_typing(text: &str, at: usize) -> usize {
    let head: String = text.chars().take(at).collect();
    let rest: String = text.chars().skip(at).collect();
    let mut cursor = items(&head).len();
    if head.trim_end_matches([' ', '\t']).ends_with('\n') && cursor > 0 && !rest.trim().is_empty() {
        cursor += 1;
    }
    cursor
}

/// `t` ends a sentence (or is a break).
fn ends(t: &Tok) -> bool {
    t.brk || {
        let s = t.text.trim_end_matches(['"', '\'', ')', '”', '’']);
        s.ends_with(['.', '?', '!', '…'])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(start: i64, text: &str) -> KClip {
        let words: Vec<String> = text.split_whitespace().map(str::to_string).collect();
        let times = (0..words.len() as i64)
            .map(|i| (start + i * 100, start + i * 100 + 90))
            .collect();
        KClip {
            partial: false,
            start_ms: start,
            words,
            times,
        }
    }

    fn shown(d: &Doc) -> String {
        let doc = d.build();
        let (pieces, caret) = d.pieces(&doc);
        let mut s = String::new();
        for (n, &i) in pieces.iter().enumerate() {
            if n == caret {
                s.push_str("| ");
            }
            if doc[i].brk {
                s.push_str("¶ ");
            } else {
                s.push_str(&doc[i].text);
                s.push(' ');
            }
        }
        if caret == pieces.len() {
            s.push('|');
        }
        s.trim_end().to_string()
    }

    /// The index in `d.build()` of the shown word `w`.
    fn find(d: &Doc, w: &str) -> usize {
        let doc = d.build();
        doc.iter()
            .position(|t| d.shown(t) && t.text == w)
            .expect("word")
    }

    #[test]
    fn speech_goes_at_the_end_of_loaded_text() {
        let mut d = Doc::default();
        d.load("One two.\nThree four.", usize::MAX, 0);
        assert_eq!(shown(&d), "One two. ¶ Three four. |");
        d.clips = vec![clip(1000, "Five six.")];
        assert_eq!(shown(&d), "One two. ¶ Three four. Five six. |");
        assert_eq!(d.text(), "One two.\n\nThree four. Five six.");
    }

    #[test]
    fn speech_mid_sentence_is_lowercased_and_fitted() {
        let mut d = Doc::default();
        d.load("The quick fox jumps.", usize::MAX, 0);
        let doc = d.build();
        let fox = find(&d, "fox");
        assert!(d.move_cursor(&doc, fox, false, || 500));
        d.clips = vec![clip(1000, "Brown.")];
        assert_eq!(shown(&d), "The quick brown | fox jumps.");
        // Names keep their capital.
        let mut d = Doc::default();
        d.load("Ask about it.", usize::MAX, 0);
        let doc = d.build();
        let about = find(&d, "about");
        d.move_cursor(&doc, about, false, || 500);
        d.clips = vec![clip(1000, "I.")];
        assert_eq!(shown(&d), "Ask I | about it.");
    }

    #[test]
    fn moves_without_speech_replace_each_other() {
        let mut d = Doc::default();
        d.load("a b c d", usize::MAX, 0);
        for w in ["b", "c", "d", "a"] {
            let doc = d.build();
            let i = find(&d, w);
            d.move_cursor(&doc, i, false, || 10);
        }
        assert_eq!(d.inserts.len(), 1);
        assert_eq!(shown(&d), "| a b c d");
        d.clips = vec![clip(1000, "Zero")];
        assert_eq!(shown(&d), "Zero | a b c d");
    }

    #[test]
    fn breaks_and_sentence_backspace() {
        let mut d = Doc::default();
        d.load("", usize::MAX, 0);
        d.clips = vec![clip(0, "First one. Second one")];
        let doc = d.build();
        assert!(d.paragraph_break(&doc, || 1000));
        // Not two in a row.
        let doc = d.build();
        assert!(!d.paragraph_break(&doc, || 1001));
        d.clips.push(clip(2000, "Third."));
        assert_eq!(shown(&d), "First one. Second one ¶ Third. |");
        let doc = d.build();
        assert!(d.delete_sentence(&doc));
        assert_eq!(shown(&d), "First one. Second one ¶ |");
        let doc = d.build();
        assert!(d.delete_sentence(&doc));
        assert_eq!(shown(&d), "First one. Second one |");
        let doc = d.build();
        assert!(d.delete_sentence(&doc));
        assert_eq!(shown(&d), "First one. |");
        let doc = d.build();
        assert!(d.delete_sentence(&doc));
        assert_eq!(shown(&d), "|");
        let doc = d.build();
        assert!(!d.delete_sentence(&doc));
    }

    #[test]
    fn deleting_words_across_a_break_joins_once() {
        let mut d = Doc::default();
        d.load("a b\nc d\ne f", usize::MAX, 0);
        let doc = d.build();
        let (pieces, _) = d.pieces(&doc);
        // "c d" and both breaks around it.
        let picked: Vec<&Tok> = pieces[2..6].iter().map(|&i| &doc[i]).collect();
        d.delete(&picked);
        assert_eq!(shown(&d), "a b e f |");
        assert_eq!(d.text(), "a b e f");
    }

    #[test]
    fn text_and_caret_round_trip_through_typing() {
        let mut d = Doc::default();
        d.load("One two.\nThree four.", 3, 0);
        let doc = d.build();
        assert_eq!(shown(&d), "One two. ¶ | Three four.");
        let (text, at) = d.text_and_caret(&doc);
        assert_eq!(text, "One two.\n\nThree four.");
        assert_eq!(&text[at..], "Three four.");
        let cursor = cursor_after_typing(&text, at);
        d.load(&text, cursor, 5000);
        assert_eq!(shown(&d), "One two. ¶ | Three four.");
        // At the end of the first paragraph.
        let cursor = cursor_after_typing(&text, "One two.".len());
        d.load(&text, cursor, 5000);
        assert_eq!(shown(&d), "One two. | ¶ Three four.");
    }

    #[test]
    fn files_unwrap_into_paragraphs() {
        assert_eq!(
            unwrap_file("one\ntwo\n\n\nthree\n"),
            "one two\nthree".to_string()
        );
    }
}

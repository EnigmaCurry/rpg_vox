//! The cast file: which voice each character in the script is played by,
//! and the handful of production numbers that decide pacing.
//!
//! It is TOML, written by hand, and keyed on the speaker labels that are
//! already in the script — put through [`normalize_speaker`] on both
//! sides, so whether the script writes `**Voss:**` or `**VOSS:**` and
//! whether the cast file writes `GM — TABLE` or `GM-TABLE` stops being
//! something anybody has to get right.
//!
//! ```toml
//! [production]
//! language = "English"
//!
//! [cast."GM / NARRATOR"]
//! sample = "voices/narrator.wav"
//!
//! [cast."GM — TABLE"]
//! same_as = "GM / NARRATOR"
//!
//! [cast."FE"]
//! server_voice = "old-man"
//! ```
//!
//! A character is cast one of three ways: `sample` points at reference
//! audio on disk, which is registered with the clone server under a name
//! derived from the file's own contents; `server_voice` names a voice
//! already registered there; `same_as` points at another character, for
//! the parts that share a performer.
//!
//! Content-addressing the sample-based names is what makes a render
//! repeatable. The same wav always registers under the same name, so a
//! second run finds it already there and uploads nothing, and a *changed*
//! wav gets a different name — which also invalidates every cached
//! utterance that voice had synthesized, because the cache key contains
//! it. Nothing has to be cleared by hand.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::script::{normalize_speaker, Script, SpeakerCount};

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CastFile {
    #[serde(default)]
    pub production: Production,
    /// Keyed on the speaker label as the script writes it.
    #[serde(default)]
    pub cast: BTreeMap<String, CastEntry>,
    /// Keyed on a substring of a performance direction. Merged over the
    /// built-in table unless that is turned off.
    #[serde(default)]
    pub directions: BTreeMap<String, DirectionEffect>,
}

/// Every gap in the play, in seconds, plus the two numbers that decide
/// how tightly the synthesized utterances butt up against each other.
///
/// The defaults are what the pacing of a read-aloud drama wants rather
/// than anything the format dictates: a beat between paragraphs of one
/// speech, a little more when the speaker changes, two seconds at a
/// scene break, and a hole left where each `[SFX]` will eventually go.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Production {
    /// Overrides the title taken from the script's first `#` heading.
    #[serde(default)]
    pub title: Option<String>,
    /// Passed to the synthesizer for every voice that does not set its
    /// own.
    #[serde(default = "default_language")]
    pub language: String,

    #[serde(default = "d_pause")]
    pub pause_s: f32,
    #[serde(default = "d_long_pause")]
    pub long_pause_s: f32,
    /// Silence left in place of an `[SFX]` / `[MUSIC]` / `[TRANSITION]`
    /// cue this version does not produce.
    #[serde(default = "d_cue")]
    pub cue_s: f32,
    #[serde(default = "d_scene")]
    pub scene_s: f32,
    #[serde(default = "d_slug")]
    pub slug_s: f32,
    /// Between paragraphs of one speech.
    #[serde(default = "d_paragraph")]
    pub paragraph_s: f32,
    /// Between two speeches by the same character.
    #[serde(default = "d_line")]
    pub line_s: f32,
    /// Between two characters.
    #[serde(default = "d_speaker")]
    pub speaker_s: f32,

    /// Level below which the leading and trailing edges of a synthesized
    /// utterance count as silence and are cut. The server pads, and the
    /// padding is not consistent, so without this the pacing set above
    /// is advisory at best.
    #[serde(default = "d_trim_db")]
    pub trim_db: f32,
    /// How much of the silence either side to keep, so a cut never lands
    /// on the first consonant.
    #[serde(default = "d_trim_keep_ms")]
    pub trim_keep_ms: u32,
    /// Ramp applied at each utterance edge. Concatenating at a non-zero
    /// sample clicks; five milliseconds is inaudible and fixes it.
    #[serde(default = "d_fade_ms")]
    pub fade_ms: u32,
}

fn default_language() -> String {
    "English".into()
}
fn d_pause() -> f32 {
    1.0
}
fn d_long_pause() -> f32 {
    2.5
}
fn d_cue() -> f32 {
    0.8
}
fn d_scene() -> f32 {
    2.0
}
fn d_slug() -> f32 {
    0.0
}
fn d_paragraph() -> f32 {
    0.4
}
fn d_line() -> f32 {
    0.5
}
fn d_speaker() -> f32 {
    0.65
}
fn d_trim_db() -> f32 {
    -45.0
}
fn d_trim_keep_ms() -> u32 {
    40
}
fn d_fade_ms() -> u32 {
    5
}

impl Default for Production {
    fn default() -> Self {
        Self {
            title: None,
            language: default_language(),
            pause_s: d_pause(),
            long_pause_s: d_long_pause(),
            cue_s: d_cue(),
            scene_s: d_scene(),
            slug_s: d_slug(),
            paragraph_s: d_paragraph(),
            line_s: d_line(),
            speaker_s: d_speaker(),
            trim_db: d_trim_db(),
            trim_keep_ms: d_trim_keep_ms(),
            fade_ms: d_fade_ms(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CastEntry {
    /// Reference audio for a clone, relative to the cast file.
    #[serde(default)]
    pub sample: Option<PathBuf>,
    /// What the sample says, word for word. Optional, and better left
    /// out than guessed at: the clone conditions on the audio and this
    /// text *as a pair*, so a transcript that disagrees with the
    /// recording breaks it rather than refining it. Measured on a
    /// 16-second reference with a speech-to-text transcript of
    /// disfluent conversation, half the requests failed with "did not
    /// emit codec EOS" and the rest ran four times longer than the
    /// words needed; the same recording with this unset failed none.
    /// Set it only when the transcript is exact.
    #[serde(default)]
    pub ref_text: Option<String>,
    /// A voice already registered on the clone server, by name.
    #[serde(default)]
    pub server_voice: Option<String>,
    /// Another character in this file, whose voice this one shares.
    #[serde(default)]
    pub same_as: Option<String>,
    /// Overrides `production.language` for this character.
    #[serde(default)]
    pub language: Option<String>,
    /// Trim applied to every line this character says.
    #[serde(default)]
    pub gain_db: f32,
}

/// What a performance direction is allowed to do to a line.
///
/// Not much, deliberately. The clone model takes no style input — it
/// accepts an `instructions` field and ignores it — so a direction can
/// only be realized by processing the audio that comes back. Level is
/// honest; a band-pass and a little saturation is a fair reading of
/// "over vox"; anything beyond that would be pretending.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DirectionEffect {
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default)]
    pub highpass_hz: Option<f32>,
    #[serde(default)]
    pub lowpass_hz: Option<f32>,
    /// Soft-clip amount, 0.0 to 1.0.
    #[serde(default)]
    pub drive: f32,
}

/// The directions this format actually uses, read as generously as audio
/// processing allows. Overridden per key by the cast file's own
/// `[directions]` table.
pub fn builtin_directions() -> BTreeMap<String, DirectionEffect> {
    fn gain(db: f32) -> DirectionEffect {
        DirectionEffect {
            gain_db: db,
            ..Default::default()
        }
    }
    let mut m = BTreeMap::new();
    m.insert("barely audible".to_string(), gain(-14.0));
    m.insert("very quietly".to_string(), gain(-10.0));
    m.insert("whisper".to_string(), gain(-9.0));
    m.insert("under his breath".to_string(), gain(-8.0));
    m.insert("under her breath".to_string(), gain(-8.0));
    m.insert("quietly".to_string(), gain(-6.0));
    m.insert("softly".to_string(), gain(-6.0));
    m.insert("loud".to_string(), gain(3.0));
    m.insert(
        "over vox".to_string(),
        DirectionEffect {
            gain_db: -1.5,
            highpass_hz: Some(300.0),
            lowpass_hz: Some(3400.0),
            drive: 0.35,
        },
    );
    m.insert(
        "internal voice".to_string(),
        DirectionEffect {
            gain_db: -3.0,
            lowpass_hz: Some(6000.0),
            ..Default::default()
        },
    );
    m
}

/// Where the audio for one voice comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceSource {
    /// Reference audio to register, under a name derived from its bytes.
    Sample { path: PathBuf, ref_text: String },
    /// Already on the server under this name.
    Registered,
}

/// One voice, after the cast file has been resolved against the script.
#[derive(Debug, Clone)]
pub struct Voice {
    /// The name this voice answers to on the clone server. Part of every
    /// cache key, so it is also what decides when cached audio is stale.
    pub id: String,
    pub source: VoiceSource,
    pub language: String,
    pub gain_db: f32,
    /// The characters played by this voice, for the report.
    pub characters: Vec<String>,
}

/// A script and a cast file, reconciled.
#[derive(Debug)]
pub struct ResolvedCast {
    /// The pacing and trimming numbers, defaulted. Carried here so the
    /// renderer takes one resolved thing rather than a file and a script
    /// that have to be kept in step.
    pub production: Production,
    pub voices: Vec<Voice>,
    /// Normalized speaker key to an index into `voices`.
    by_key: HashMap<String, usize>,
    /// Characters the script has and the cast file does not. Rendering
    /// cannot proceed with any of these; `radio_drama cast` writes a file
    /// that has them all.
    pub uncast: Vec<SpeakerCount>,
    /// Directions, built-ins merged under the cast file's own.
    pub directions: BTreeMap<String, DirectionEffect>,
}

impl ResolvedCast {
    pub fn voice_for(&self, key: &str) -> Option<&Voice> {
        self.by_key.get(key).map(|&i| &self.voices[i])
    }

    /// The effect for a direction: the longest matching key wins, so
    /// "very quietly" is not read as "quietly" and "barely audible" is
    /// not missed for having no volume word in it at all.
    pub fn effect_for(&self, direction: &str) -> Option<(&str, &DirectionEffect)> {
        let d = direction.to_lowercase();
        self.directions
            .iter()
            .filter(|(k, _)| d.contains(k.to_lowercase().as_str()))
            .max_by_key(|(k, _)| k.len())
            .map(|(k, v)| (k.as_str(), v))
    }
}

impl CastFile {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading cast file {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing cast file {}", path.display()))
    }

    /// Reconcile this cast file with a script. `base` is the directory
    /// the cast file's `sample` paths are relative to.
    pub fn resolve(&self, script: &Script, base: &Path) -> Result<ResolvedCast> {
        // Key the entries the same way the script's speakers are keyed,
        // and refuse a file that casts one character twice under two
        // spellings — silently picking one would be a coin toss.
        let mut entries: HashMap<String, (&str, &CastEntry)> = HashMap::new();
        for (written, entry) in &self.cast {
            let key = normalize_speaker(written);
            if let Some((first, _)) = entries.insert(key.clone(), (written.as_str(), entry)) {
                bail!(
                    "cast file casts {key} twice, as {first:?} and {written:?}; \
                     the two names differ only in case, spacing or dash"
                );
            }
        }

        let mut voices: Vec<Voice> = Vec::new();
        let mut by_key: HashMap<String, usize> = HashMap::new();
        let mut uncast = Vec::new();
        // Voice id to index, so two characters cast from the same wav or
        // the same `server_voice` share one voice rather than uploading
        // and caching twice.
        let mut by_id: HashMap<String, usize> = HashMap::new();

        for speaker in script.speakers() {
            let Some((written, entry)) = resolve_aliases(&entries, &speaker.key)? else {
                uncast.push(speaker);
                continue;
            };
            // An entry that names no voice is a part the cast file has
            // listed and not filled in — which is exactly what
            // `radio_drama cast` writes — so it counts as uncast rather
            // than as a malformed file.
            let Some((id, source)) = voice_identity(entry, base, written)? else {
                uncast.push(speaker);
                continue;
            };
            let language = entry
                .language
                .clone()
                .unwrap_or_else(|| self.production.language.clone());
            let idx = match by_id.get(&id) {
                Some(&idx) => idx,
                None => {
                    voices.push(Voice {
                        id: id.clone(),
                        source,
                        language,
                        gain_db: entry.gain_db,
                        characters: Vec::new(),
                    });
                    by_id.insert(id, voices.len() - 1);
                    voices.len() - 1
                }
            };
            voices[idx].characters.push(speaker.display.clone());
            by_key.insert(speaker.key.clone(), idx);
        }

        let mut directions = builtin_directions();
        for (k, v) in &self.directions {
            directions.insert(k.to_lowercase(), v.clone());
        }

        Ok(ResolvedCast {
            production: self.production.clone(),
            voices,
            by_key,
            uncast,
            directions,
        })
    }
}

/// Follow `same_as` to the entry that actually names a voice. Bounded so
/// a file that points two characters at each other says so instead of
/// hanging.
fn resolve_aliases<'a>(
    entries: &HashMap<String, (&'a str, &'a CastEntry)>,
    key: &str,
) -> Result<Option<(&'a str, &'a CastEntry)>> {
    let mut seen = vec![key.to_string()];
    let mut at = key.to_string();
    loop {
        let Some(&(written, entry)) = entries.get(&at) else {
            if seen.len() == 1 {
                return Ok(None);
            }
            bail!(
                "cast file points {:?} at {at:?}, which it does not cast",
                seen[seen.len() - 2]
            );
        };
        let Some(next) = entry.same_as.as_deref() else {
            return Ok(Some((written, entry)));
        };
        let next = normalize_speaker(next);
        if seen.contains(&next) {
            bail!(
                "cast file has a same_as loop: {}",
                seen.join(" -> ") + " -> " + &next
            );
        }
        seen.push(next.clone());
        at = next;
    }
}

/// The server-side name for a cast entry's voice, and where its audio
/// comes from.
///
/// `None` when the entry names no voice at all: a part listed and not yet
/// cast, which is what the scaffold writes and what the report is for.
fn voice_identity(
    entry: &CastEntry,
    base: &Path,
    written: &str,
) -> Result<Option<(String, VoiceSource)>> {
    match (&entry.sample, &entry.server_voice) {
        (Some(_), Some(_)) => Err(anyhow!(
            "cast entry {written:?} sets both sample and server_voice; it can have one"
        )),
        (None, None) => Ok(None),
        (None, Some(name)) => {
            if name.trim().is_empty() {
                bail!("cast entry {written:?} has an empty server_voice");
            }
            Ok(Some((name.trim().to_string(), VoiceSource::Registered)))
        }
        (Some(rel), None) => {
            let path = if rel.is_absolute() {
                rel.clone()
            } else {
                base.join(rel)
            };
            let bytes = std::fs::read(&path).with_context(|| {
                format!(
                    "reading reference audio for {written:?}: {}",
                    path.display()
                )
            })?;
            let ref_text = entry.ref_text.clone().unwrap_or_default();
            // The sample's bytes and its ref_text together: both change
            // the voice the server builds, so both have to change the
            // name it is built under.
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            hasher.update([0u8]);
            hasher.update(ref_text.as_bytes());
            let digest = hasher.finalize();
            let id = format!("rd-{}", hex16(&digest));
            Ok(Some((id, VoiceSource::Sample { path, ref_text })))
        }
    }
}

fn hex16(bytes: &[u8]) -> String {
    bytes[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Write a cast file for a script that has none, with every character in
/// it and nothing cast — a form to fill in rather than a guess at who
/// should sound like what. The two GM labels are the one exception: they
/// are the same person at the table, so the out-of-character one is
/// pointed at the narrator.
pub fn scaffold(script: &Script, existing: Option<&CastFile>) -> String {
    let speakers = script.speakers();
    let narrator = speakers
        .iter()
        .find(|s| s.key.contains("NARRATOR"))
        .map(|s| s.display.clone());

    let mut out = String::new();
    out.push_str("# Cast for ");
    out.push_str(
        script
            .title
            .as_deref()
            .or(script.subtitle.as_deref())
            .unwrap_or("this radio play"),
    );
    out.push_str(
        "\n#\n\
         # Give every character below a voice, then run `radio_drama render`.\n\
         # A character takes one of:\n\
         #\n\
         #   sample       = \"voices/name.wav\"   reference audio to clone\n\
         #   ref_text     = \"what it says\"      only if word-for-word exact\n\
         #   server_voice = \"old-man\"           a voice already on the server\n\
         #   same_as      = \"ANOTHER CHARACTER\" share another part's voice\n\
         #\n\
         # and may also set `language` and `gain_db`.\n\n",
    );

    let p = existing.map(|c| c.production.clone()).unwrap_or_default();
    out.push_str("[production]\n");
    if let Some(title) = &p.title {
        out.push_str(&format!("title = {}\n", toml_string(title)));
    }
    out.push_str(&format!("language = {}\n", toml_string(&p.language)));
    out.push_str("\n# Every gap in the play, in seconds.\n");
    for (k, v) in [
        ("paragraph_s", p.paragraph_s),
        ("line_s", p.line_s),
        ("speaker_s", p.speaker_s),
        ("pause_s", p.pause_s),
        ("long_pause_s", p.long_pause_s),
        ("cue_s", p.cue_s),
        ("scene_s", p.scene_s),
        ("slug_s", p.slug_s),
    ] {
        out.push_str(&format!("{k} = {v}\n"));
    }
    out.push('\n');

    for speaker in &speakers {
        out.push_str(&format!(
            "# {} \u{2014} {} line{}, {} word{}\n",
            speaker.display,
            speaker.lines,
            if speaker.lines == 1 { "" } else { "s" },
            speaker.words,
            if speaker.words == 1 { "" } else { "s" },
        ));
        out.push_str(&format!("[cast.{}]\n", toml_string(&speaker.display)));

        let known = existing.and_then(|c| {
            c.cast
                .iter()
                .find(|(written, _)| normalize_speaker(written) == speaker.key)
                .map(|(_, e)| e)
        });
        match known {
            Some(entry) => {
                if let Some(same) = &entry.same_as {
                    out.push_str(&format!("same_as = {}\n", toml_string(same)));
                }
                if let Some(sample) = &entry.sample {
                    out.push_str(&format!(
                        "sample = {}\n",
                        toml_string(&sample.display().to_string())
                    ));
                }
                if let Some(ref_text) = &entry.ref_text {
                    out.push_str(&format!("ref_text = {}\n", toml_string(ref_text)));
                }
                if let Some(sv) = &entry.server_voice {
                    out.push_str(&format!("server_voice = {}\n", toml_string(sv)));
                }
                if let Some(lang) = &entry.language {
                    out.push_str(&format!("language = {}\n", toml_string(lang)));
                }
                if entry.gain_db != 0.0 {
                    out.push_str(&format!("gain_db = {}\n", entry.gain_db));
                }
            }
            // The game master narrating and the game master adjudicating
            // at the table are one person, so the second is cast as the
            // first rather than left as a part to fill.
            None if speaker.key.starts_with("GM") && !speaker.key.contains("NARRATOR") => {
                match &narrator {
                    Some(n) => out.push_str(&format!("same_as = {}\n", toml_string(n))),
                    None => out.push_str("# sample = \"voices/.wav\"\n"),
                }
            }
            None => out.push_str("# sample = \"voices/.wav\"\n"),
        }
        out.push('\n');
    }

    out.push_str(
        "# Performance directions. The clone model takes no style input, so a\n\
         # direction can only be honoured by processing the audio it returns.\n\
         # The keys below are matched as substrings of the parenthetical in the\n\
         # script, longest key first; these are the built-in defaults, and any\n\
         # you uncomment replaces the one of that name.\n",
    );
    let used: Vec<String> = {
        let mut v: Vec<String> = script
            .lines()
            .filter_map(|l| l.direction.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        v.sort();
        v
    };
    for (key, eff) in builtin_directions() {
        out.push_str(&format!("#\n# [directions.{}]\n", toml_string(&key)));
        out.push_str(&format!("# gain_db = {}\n", eff.gain_db));
        if let Some(hz) = eff.highpass_hz {
            out.push_str(&format!("# highpass_hz = {hz}\n"));
        }
        if let Some(hz) = eff.lowpass_hz {
            out.push_str(&format!("# lowpass_hz = {hz}\n"));
        }
        if eff.drive != 0.0 {
            out.push_str(&format!("# drive = {}\n", eff.drive));
        }
    }
    if !used.is_empty() {
        out.push_str("#\n# Directions this script uses:\n");
        for d in used {
            out.push_str(&format!("#   ({d})\n"));
        }
    }
    out
}

/// TOML basic string. Used for keys as well as values, which is legal
/// for both and is the only form that survives a character called
/// `GM / NARRATOR`.
fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::ParseOptions;

    fn script(md: &str) -> Script {
        Script::parse(md, &ParseOptions::default())
    }

    fn cast(toml_src: &str) -> CastFile {
        toml::from_str(toml_src).expect("cast file parses")
    }

    #[test]
    fn an_uncast_character_is_reported_not_guessed() {
        let s = script("**MAX:** One.\n\n**VOSS:** Two.\n");
        let c = cast("[cast.MAX]\nserver_voice = \"old-man\"\n");
        let r = c.resolve(&s, Path::new(".")).unwrap();
        assert_eq!(r.voices.len(), 1);
        assert_eq!(
            r.uncast.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(),
            vec!["VOSS"]
        );
    }

    #[test]
    fn matches_across_case_spacing_and_dash() {
        let s = script("**Voss:** One.\n\n**GM \u{2014} TABLE:** Roll it.\n");
        let c = cast(
            "[cast.VOSS]\nserver_voice = \"a\"\n\n[cast.\"gm-table\"]\nserver_voice = \"b\"\n",
        );
        let r = c.resolve(&s, Path::new(".")).unwrap();
        assert!(r.uncast.is_empty());
        assert_eq!(r.voice_for("VOSS").unwrap().id, "a");
        assert_eq!(r.voice_for("GM-TABLE").unwrap().id, "b");
    }

    #[test]
    fn same_as_shares_one_voice() {
        let s = script("**GM / NARRATOR:** One.\n\n**GM \u{2014} TABLE:** Roll it.\n");
        let c = cast(
            "[cast.\"GM / NARRATOR\"]\nserver_voice = \"narr\"\n\n\
             [cast.\"GM \u{2014} TABLE\"]\nsame_as = \"GM / NARRATOR\"\n",
        );
        let r = c.resolve(&s, Path::new(".")).unwrap();
        assert_eq!(r.voices.len(), 1);
        assert_eq!(r.voices[0].characters.len(), 2);
        assert_eq!(r.voice_for("GM-TABLE").unwrap().id, "narr");
    }

    #[test]
    fn two_characters_on_one_server_voice_are_one_voice() {
        let s = script("**A:** One.\n\n**B:** Two.\n");
        let c = cast("[cast.A]\nserver_voice = \"x\"\n\n[cast.B]\nserver_voice = \"x\"\n");
        let r = c.resolve(&s, Path::new(".")).unwrap();
        assert_eq!(r.voices.len(), 1);
    }

    #[test]
    fn a_same_as_loop_is_an_error_not_a_hang() {
        let s = script("**A:** One.\n");
        let c = cast("[cast.A]\nsame_as = \"B\"\n\n[cast.B]\nsame_as = \"A\"\n");
        let err = c.resolve(&s, Path::new(".")).unwrap_err().to_string();
        assert!(err.contains("same_as loop"), "{err}");
    }

    #[test]
    fn same_as_pointing_nowhere_says_so() {
        let s = script("**A:** One.\n");
        let c = cast("[cast.A]\nsame_as = \"NOBODY\"\n");
        let err = c.resolve(&s, Path::new(".")).unwrap_err().to_string();
        assert!(err.contains("does not cast"), "{err}");
    }

    #[test]
    fn one_character_cast_twice_is_an_error() {
        let s = script("**VOSS:** One.\n");
        let c = cast("[cast.VOSS]\nserver_voice = \"a\"\n\n[cast.Voss]\nserver_voice = \"b\"\n");
        let err = c.resolve(&s, Path::new(".")).unwrap_err().to_string();
        assert!(err.contains("twice"), "{err}");
    }

    #[test]
    fn sample_and_server_voice_together_is_an_error() {
        let s = script("**A:** One.\n");
        let c = cast("[cast.A]\nsample = \"a.wav\"\nserver_voice = \"b\"\n");
        let err = c.resolve(&s, Path::new(".")).unwrap_err().to_string();
        assert!(err.contains("one"), "{err}");
    }

    #[test]
    fn the_longest_direction_key_wins() {
        let s = script("**A:** One.\n");
        let c = cast("[cast.A]\nserver_voice = \"x\"\n");
        let r = c.resolve(&s, Path::new(".")).unwrap();
        assert_eq!(r.effect_for("very quietly").unwrap().0, "very quietly");
        assert_eq!(r.effect_for("quietly, to Max").unwrap().0, "quietly");
        assert_eq!(r.effect_for("whispering").unwrap().0, "whisper");
        assert!(r.effect_for("with ritual gravity").is_none());
    }

    #[test]
    fn the_cast_file_replaces_a_builtin_direction() {
        let s = script("**A:** One.\n");
        let c =
            cast("[cast.A]\nserver_voice = \"x\"\n\n[directions.\"over vox\"]\ngain_db = -20.0\n");
        let r = c.resolve(&s, Path::new(".")).unwrap();
        let (_, eff) = r.effect_for("over vox").unwrap();
        assert_eq!(eff.gain_db, -20.0);
        assert_eq!(eff.highpass_hz, None);
    }

    #[test]
    fn a_scaffold_round_trips_and_names_every_part() {
        let s = script(
            "**GM / NARRATOR:** One.\n\n**GM \u{2014} TABLE:** Roll it.\n\n**VOSS** *(over vox)*: Two.\n",
        );
        let text = scaffold(&s, None);
        let parsed: CastFile = toml::from_str(&text).expect("scaffold is valid toml");
        assert_eq!(parsed.cast.len(), 3);
        // Table talk is cast as the narrator; the rest is left to fill in.
        assert_eq!(
            parsed.cast["GM \u{2014} TABLE"].same_as.as_deref(),
            Some("GM / NARRATOR")
        );
        assert!(parsed.cast["VOSS"].sample.is_none());
        assert!(text.contains("(over vox)"));

        // Nothing is cast yet, including the part pointed at the
        // narrator: an alias to an unfilled part is itself unfilled.
        let r = parsed.resolve(&s, Path::new(".")).unwrap();
        assert_eq!(r.uncast.len(), 3);
    }

    #[test]
    fn a_scaffold_keeps_what_was_already_cast() {
        let s = script("**MAX:** One.\n\n**VOSS:** Two.\n");
        let first = cast("[cast.MAX]\nserver_voice = \"old-man\"\ngain_db = -2.0\n");
        let text = scaffold(&s, Some(&first));
        let parsed: CastFile = toml::from_str(&text).unwrap();
        assert_eq!(parsed.cast["MAX"].server_voice.as_deref(), Some("old-man"));
        assert_eq!(parsed.cast["MAX"].gain_db, -2.0);
        assert!(parsed.cast["VOSS"].server_voice.is_none());
    }
}

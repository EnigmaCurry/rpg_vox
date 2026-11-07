//! Laying a score under the dialogue.
//!
//! The speech-only render leaves the music alone and writes down where
//! it would go: every `[SFX]`, `[MUSIC]` and `[TRANSITION]` cue in the
//! script is in the manifest with the timestamps of the silence left for
//! it. This is the other half — given cue files and a `music.toml`
//! saying which goes where, it reads that mix back, puts the score under
//! it, and writes a stereo master.
//!
//! Four things decide how it sounds, and all four come from how a
//! composer's cue sheet talks about underscoring rather than from
//! anything convenient:
//!
//! * **A cue underscores a scene.** Not a moment — the cue sheet says
//!   "atmospheric underscoring, not finished dialogue mixes", and the
//!   cue names map onto scene titles. So a cue is placed against a scene
//!   and fills it.
//!
//! * **It loops to fit.** The cues here are 38 to 65 seconds and the
//!   scenes run to five minutes, so filling one means repeating, which
//!   means crossfading: butting a loop point against itself clicks, and
//!   a couple of seconds of overlap is what makes a 52-second bed sound
//!   like five minutes of underscore.
//!
//! * **It ducks under speech.** [`duck`] drives the music's level from
//!   the dialogue's own envelope, fast to get out of the way and slow to
//!   come back. This is the difference between a score and a thing you
//!   have to talk over.
//!
//! * **The master is stereo.** The dialogue is one mono track and goes
//!   up the middle; the cues are stereo and keep their width. A mono
//!   master would throw away the only part of the production that has
//!   any, which is why the speech-only version stayed mono rather than
//!   pretending.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;
use tracing::{debug, info};

use crate::audio::{self, Stereo};
use crate::dsp;
use crate::render::{ManifestEntry, Mix};

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MusicFile {
    #[serde(default)]
    pub score: Score,
    /// The cues, in no particular order — each says where it goes.
    #[serde(default, rename = "cue")]
    pub cues: Vec<Cue>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Score {
    /// Where the cue files are, relative to this file.
    #[serde(default = "d_dir")]
    pub dir: PathBuf,
    /// Level every cue sits at before ducking, in dB. The cues here
    /// arrive at a mean of −22 to −34 dBFS already, so this is trim on
    /// top of whatever the composer bounced, not a mastering stage.
    #[serde(default = "d_gain")]
    pub gain_db: f32,
    /// How far the music drops while somebody is speaking.
    #[serde(default = "d_duck")]
    pub duck_db: f32,
    /// Dialogue level that counts as speech, in dBFS.
    #[serde(default = "d_threshold")]
    pub duck_threshold_db: f32,
    /// How quickly the music gets out of the way. Short — being late to
    /// duck means the first syllable of a line is the one competing.
    #[serde(default = "d_attack")]
    pub duck_attack_ms: u32,
    /// How slowly it comes back. Long — music that surges into every
    /// gap between two lines is worse than music that stays down.
    #[serde(default = "d_release")]
    pub duck_release_ms: u32,
    #[serde(default = "d_fade_in")]
    pub fade_in_s: f32,
    #[serde(default = "d_fade_out")]
    pub fade_out_s: f32,
    /// Overlap between one pass of a looping cue and the next.
    #[serde(default = "d_loop_xf")]
    pub loop_crossfade_s: f32,
    /// Rate to master at. By default the highest rate in the
    /// production, so the score is not band-limited to what the speech
    /// needed.
    #[serde(default)]
    pub sample_rate: Option<u32>,
}

fn d_dir() -> PathBuf {
    PathBuf::from("music")
}
fn d_gain() -> f32 {
    -6.0
}
fn d_duck() -> f32 {
    -11.0
}
fn d_threshold() -> f32 {
    -40.0
}
fn d_attack() -> u32 {
    80
}
fn d_release() -> u32 {
    900
}
fn d_fade_in() -> f32 {
    2.0
}
fn d_fade_out() -> f32 {
    3.0
}
fn d_loop_xf() -> f32 {
    2.5
}

impl Default for Score {
    fn default() -> Self {
        Self {
            dir: d_dir(),
            gain_db: d_gain(),
            duck_db: d_duck(),
            duck_threshold_db: d_threshold(),
            duck_attack_ms: d_attack(),
            duck_release_ms: d_release(),
            fade_in_s: d_fade_in(),
            fade_out_s: d_fade_out(),
            loop_crossfade_s: d_loop_xf(),
            sample_rate: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cue {
    /// The audio file, relative to `score.dir`.
    pub file: PathBuf,
    /// The scene this underscores, matched case-insensitively as a
    /// substring of the scene's heading. The usual way to place a cue:
    /// it follows the script rather than a stopwatch, so re-rendering
    /// after an edit moves the music with the scene.
    #[serde(default)]
    pub scene: Option<String>,
    /// Explicit placement, in seconds, for a cue that is not a scene
    /// bed. Overrides `scene`.
    #[serde(default)]
    pub at_s: Option<f64>,
    /// Explicit length, in seconds. Without it a cue fills its scene,
    /// or plays once from `at_s`.
    #[serde(default)]
    pub for_s: Option<f64>,
    /// Trim for this cue alone, on top of `score.gain_db`.
    #[serde(default)]
    pub gain_db: f32,
    /// Leave this cue at full level under speech. For a stinger in a
    /// gap, where ducking to nothing is the wrong answer.
    #[serde(default)]
    pub no_duck: bool,
    /// Play once rather than filling the span.
    #[serde(default)]
    pub once: bool,
}

/// A cue, resolved against a rendered mix: which file, from when, for
/// how long.
#[derive(Debug, Clone)]
pub struct Placement {
    pub file: PathBuf,
    pub scene: Option<String>,
    pub start_s: f64,
    pub seconds: f64,
    pub gain_db: f32,
    pub no_duck: bool,
    pub once: bool,
    /// How many passes of the cue the span needed.
    pub loops: usize,
}

impl MusicFile {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading music file {}", path.display()))?;
        let parsed: Self = toml::from_str(&text)
            .with_context(|| format!("parsing music file {}", path.display()))?;
        Ok(parsed)
    }

    /// Work out where every cue goes.
    ///
    /// A scene-placed cue runs from its scene's heading to the next
    /// scene's, which is what makes the score follow the script: edit a
    /// line and the bed moves with it rather than drifting out of sync
    /// with a timestamp somebody wrote down once.
    pub fn resolve(
        &self,
        entries: &[ManifestEntry],
        total_s: f64,
        base: &Path,
    ) -> Result<Vec<Placement>> {
        let scenes: Vec<(&str, f64, f64)> = {
            let headings: Vec<(&str, f64)> = entries
                .iter()
                .filter(|e| e.kind == "scene")
                .filter_map(|e| e.heading.as_deref().map(|h| (h, e.start_s)))
                .collect();
            headings
                .iter()
                .enumerate()
                .map(|(i, &(h, start))| {
                    let end = headings.get(i + 1).map_or(total_s, |&(_, s)| s);
                    (h, start, end)
                })
                .collect()
        };

        let mut out = Vec::new();
        for cue in &self.cues {
            let path = base.join(&self.score.dir).join(&cue.file);
            if !path.exists() {
                bail!("cue file {} does not exist", path.display());
            }
            let (scene, start_s, seconds) = match (&cue.scene, cue.at_s) {
                (_, Some(at)) => (None, at, cue.for_s.unwrap_or(f64::INFINITY)),
                (Some(name), None) => {
                    let needle = name.to_lowercase();
                    let hits: Vec<&(&str, f64, f64)> = scenes
                        .iter()
                        .filter(|(h, _, _)| h.to_lowercase().contains(&needle))
                        .collect();
                    match hits.as_slice() {
                        [] => bail!(
                            "cue {} is placed in scene {name:?}, which this play has no scene matching. \
                             It has: {}",
                            cue.file.display(),
                            scenes
                                .iter()
                                .map(|(h, _, _)| *h)
                                .collect::<Vec<_>>()
                                .join("; ")
                        ),
                        // Ambiguity is the operator's to resolve: the
                        // alternative is picking one and putting the
                        // score under the wrong half of the play.
                        [_, _, ..] => bail!(
                            "cue {} is placed in scene {name:?}, which matches {} scenes: {}",
                            cue.file.display(),
                            hits.len(),
                            hits.iter()
                                .map(|(h, _, _)| *h)
                                .collect::<Vec<_>>()
                                .join("; ")
                        ),
                        [&(h, start, end)] => {
                            (Some(h.to_string()), start, cue.for_s.unwrap_or(end - start))
                        }
                    }
                }
                (None, None) => bail!(
                    "cue {} says neither a scene nor an at_s",
                    cue.file.display()
                ),
            };
            out.push(Placement {
                file: path,
                scene,
                start_s,
                seconds,
                gain_db: self.score.gain_db + cue.gain_db,
                no_duck: cue.no_duck,
                once: cue.once,
                loops: 0,
            });
        }
        out.sort_by(|a, b| a.start_s.partial_cmp(&b.start_s).expect("times are finite"));
        Ok(out)
    }
}

/// Repeat `source` with crossfaded loop points until it is `frames`
/// long, then fade the ends.
///
/// Crossfading rather than butting the loop point against itself: a cue
/// does not end on the sample it started on, so a hard splice clicks
/// once a minute for the length of the scene.
fn fill(source: &Stereo, frames: usize, crossfade: usize, once: bool) -> (Stereo, usize) {
    let mut out = Stereo::silence(frames);
    if source.frames() == 0 || frames == 0 {
        return (out, 0);
    }
    let len = source.frames();
    let crossfade = crossfade.min(len / 2);
    // Each pass after the first starts `crossfade` frames before the
    // previous one ends, so the useful stride is shorter than the cue.
    let stride = (len - crossfade).max(1);

    let mut at = 0usize;
    let mut passes = 0usize;
    while at < frames {
        for i in 0..len {
            let dst = at + i;
            if dst >= frames {
                break;
            }
            // Fade this pass in over its head and out over its tail,
            // but only where it actually overlaps a neighbour.
            let mut g = 1.0f32;
            if crossfade > 0 {
                if passes > 0 && i < crossfade {
                    g *= i as f32 / crossfade as f32;
                }
                if i >= len - crossfade && at + stride < frames {
                    g *= (len - i) as f32 / crossfade as f32;
                }
            }
            out.left[dst] += source.left[i] * g;
            out.right[dst] += source.right[i] * g;
        }
        passes += 1;
        at += stride;
        if once {
            break;
        }
    }
    (out, passes)
}

/// A gain envelope that pulls the music down while the dialogue is
/// speaking.
///
/// Classic broadcast ducking rather than anything clever: a threshold on
/// the dialogue's own level, and a one-pole smoother on the *gain* with
/// separate attack and release so the move itself is inaudible. Driving
/// the gain continuously from the dialogue level instead would make the
/// music breathe with every syllable.
pub fn duck(
    dialogue: &[f32],
    rate: u32,
    threshold_db: f32,
    duck_db: f32,
    attack_ms: u32,
    release_ms: u32,
) -> Vec<f32> {
    let threshold = dsp::db_to_gain(threshold_db);
    let ducked = dsp::db_to_gain(duck_db);
    // One-pole coefficients: the fraction of the remaining distance
    // closed per sample, from the time constant.
    let coeff = |ms: u32| -> f32 {
        let samples = (ms as f32 * rate as f32 / 1000.0).max(1.0);
        1.0 - (-1.0 / samples).exp()
    };
    let (ka, kr) = (coeff(attack_ms), coeff(release_ms));

    // Peak over a short window, so one quiet sample inside a word does
    // not read as a gap.
    let window = (rate as usize / 100).max(1); // 10 ms
    let mut gain = 1.0f32;
    let mut out = Vec::with_capacity(dialogue.len());
    for block in dialogue.chunks(window) {
        let peak = block.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let target = if peak > threshold { ducked } else { 1.0 };
        for _ in block {
            // Falling toward the duck is the attack; rising back is the
            // release.
            let k = if target < gain { ka } else { kr };
            gain += (target - gain) * k;
            out.push(gain);
        }
    }
    out
}

/// Read the cues, lay them under the dialogue, and return the stereo
/// master.
pub fn score(mix: &Mix, music: &MusicFile, base: &Path) -> Result<Scored> {
    let placements = music.resolve(&mix.entries, mix.duration_s(), base)?;
    if placements.is_empty() {
        bail!("the music file names no cues");
    }

    // Master at the highest rate in the production rather than the
    // dialogue's, so a 32 kHz score is not cut down to the 12 kHz of
    // bandwidth speech happened to need.
    let mut cues: Vec<(usize, Stereo, u32)> = Vec::new();
    let mut rate = mix.sample_rate;
    for (i, placement) in placements.iter().enumerate() {
        let decoded = audio::decode_stereo(&placement.file)?;
        debug!(
            file = %placement.file.display(),
            rate = decoded.rate,
            seconds = decoded.audio.frames() as f32 / decoded.rate as f32,
            "read cue"
        );
        rate = rate.max(decoded.rate);
        cues.push((i, decoded.audio, decoded.rate));
    }
    let target = music.score.sample_rate.unwrap_or(rate);
    info!(target, "mastering rate");

    // Dialogue first: one mono track, resampled if the master is
    // running faster, and it goes up the middle of the image.
    let dialogue = audio::resample_mono(&mix.samples, mix.sample_rate, target)?;
    let frames = dialogue.len();
    let mut out = Stereo::silence(frames);
    out.left.copy_from_slice(&dialogue);
    out.right.copy_from_slice(&dialogue);

    let envelope = duck(
        &dialogue,
        target,
        music.score.duck_threshold_db,
        music.score.duck_db,
        music.score.duck_attack_ms,
        music.score.duck_release_ms,
    );

    let mut resolved = placements.clone();
    for (i, cue_audio, cue_rate) in cues {
        let placement = &mut resolved[i];
        let source = audio::resample_stereo(&cue_audio, cue_rate, target)?;
        let start = (placement.start_s * target as f64).round() as usize;
        if start >= frames {
            info!(
                file = %placement.file.display(),
                "cue starts past the end of the play; skipped"
            );
            placement.seconds = 0.0;
            continue;
        }
        let want = if placement.seconds.is_finite() {
            (placement.seconds * target as f64).round() as usize
        } else {
            frames - start
        };
        let want = want.min(frames - start);
        let (mut bed, passes) = fill(
            &source,
            want,
            (music.score.loop_crossfade_s * target as f32) as usize,
            placement.once,
        );
        placement.loops = passes;
        placement.seconds = want as f64 / target as f64;

        let fade_in = (music.score.fade_in_s * target as f32) as usize;
        let fade_out = (music.score.fade_out_s * target as f32) as usize;
        bed.fade_in(fade_in);
        bed.fade_out(fade_out);
        bed.gain(dsp::db_to_gain(placement.gain_db));

        for i in 0..bed.frames() {
            let dst = start + i;
            if dst >= frames {
                break;
            }
            let g = if placement.no_duck {
                1.0
            } else {
                envelope[dst]
            };
            out.left[dst] += bed.left[i] * g;
            out.right[dst] += bed.right[i] * g;
        }
    }

    let limited_db = out.limit_peak(-1.0);
    Ok(Scored {
        audio: out,
        sample_rate: target,
        placements: resolved,
        limited_db,
    })
}

pub struct Scored {
    pub audio: Stereo,
    pub sample_rate: u32,
    pub placements: Vec<Placement>,
    pub limited_db: f32,
}

impl Scored {
    pub fn duration_s(&self) -> f64 {
        self.audio.frames() as f64 / self.sample_rate as f64
    }
}

/// Write a music file for a play that has none, matching cue filenames
/// against the scenes they name.
///
/// A composer's cue names and a script's scene headings are the same
/// words in a different shape — `M04_administratum.mp3` and `SCENE THREE
/// — THE ADMINISTRATUM` — so this scores the overlap in significant
/// words and takes the best unambiguous match. It is a first draft to
/// correct, not a claim: every placement is written out as an explicit
/// `scene =` line, and any cue it could not place is listed commented
/// out rather than guessed at.
pub fn scaffold(entries: &[ManifestEntry], cue_files: &[PathBuf], dir: &Path) -> String {
    let scenes: Vec<&str> = entries
        .iter()
        .filter(|e| e.kind == "scene")
        .filter_map(|e| e.heading.as_deref())
        .collect();

    // Words too common across these titles to carry any signal.
    // Deliberately NOT here: "prologue", "epilogue", "stinger" and the
    // like. They look like structural filler next to "scene", but they
    // are the only name those parts have, so dropping them left a cue
    // called `M13_epilogue.mp3` with nothing to match.
    const NOISE: &[&str] = &[
        "scene", "the", "a", "an", "of", "and", "one", "two", "three", "four", "five", "six",
        "seven", "eight", "nine", "ten", "eleven", "twelve", "mp3", "wav", "flac", "ogg", "m4a",
    ];
    fn words(s: &str) -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .map(|w| w.to_lowercase())
            .filter(|w| w.len() > 2 && !NOISE.contains(&w.as_str()))
            .collect()
    }

    let mut taken: BTreeMap<&str, PathBuf> = BTreeMap::new();
    let mut unplaced: Vec<&PathBuf> = Vec::new();
    for file in cue_files {
        let stem = file.file_stem().map(|s| s.to_string_lossy().to_string());
        let cue_words = words(stem.as_deref().unwrap_or(""));
        let best = scenes
            .iter()
            .filter(|s| !taken.contains_key(**s))
            .map(|s| {
                let scene_words = words(s);
                let hits = cue_words
                    .iter()
                    .filter(|w| scene_words.iter().any(|s| s == *w))
                    .count();
                (hits, *s)
            })
            .filter(|(hits, _)| *hits > 0)
            .max_by_key(|(hits, _)| *hits);
        match best {
            Some((_, scene)) => {
                taken.insert(scene, file.clone());
            }
            None => unplaced.push(file),
        }
    }

    let mut out = String::new();
    out.push_str(
        "# Score for this play.\n\
         #\n\
         # Each cue names the scene it underscores, matched as a substring of\n\
         # that scene's heading. A cue fills its scene, looping with a\n\
         # crossfade where the scene is longer than the cue, and ducks while\n\
         # anybody is speaking.\n\
         #\n\
         # Instead of `scene`, a cue may give `at_s` (and optionally `for_s`)\n\
         # to sit at a fixed time. Per-cue: `gain_db`, `once`, `no_duck`.\n\n",
    );
    let s = Score::default();
    out.push_str("[score]\n");
    out.push_str(&format!("dir = {:?}\n", dir.display().to_string()));
    out.push_str(&format!(
        "gain_db = {}          # every cue, before ducking\n",
        s.gain_db
    ));
    out.push_str(&format!(
        "duck_db = {}         # how far music drops under speech\n",
        s.duck_db
    ));
    out.push_str(&format!("duck_threshold_db = {}\n", s.duck_threshold_db));
    out.push_str(&format!("duck_attack_ms = {}\n", s.duck_attack_ms));
    out.push_str(&format!("duck_release_ms = {}\n", s.duck_release_ms));
    out.push_str(&format!("fade_in_s = {}\n", s.fade_in_s));
    out.push_str(&format!("fade_out_s = {}\n", s.fade_out_s));
    out.push_str(&format!("loop_crossfade_s = {}\n", s.loop_crossfade_s));
    out.push_str("# sample_rate = 32000       # default: the highest rate in the production\n\n");

    for scene in &scenes {
        match taken.get(*scene) {
            Some(file) => {
                out.push_str("[[cue]]\n");
                out.push_str(&format!("file = {:?}\n", file.display().to_string()));
                out.push_str(&format!("scene = {:?}\n\n", scene));
            }
            None => out.push_str(&format!("# no cue matched: {scene}\n\n")),
        }
    }
    if !unplaced.is_empty() {
        out.push_str("# These cues matched no scene. Give each a scene or an at_s:\n");
        for file in unplaced {
            out.push_str("#\n# [[cue]]\n");
            out.push_str(&format!("# file = {:?}\n", file.display().to_string()));
            out.push_str("# scene = \"\"\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(heading: &str, start: f64) -> ManifestEntry {
        ManifestEntry {
            kind: "scene".into(),
            start_s: start,
            end_s: start + 2.0,
            scene: Some(heading.into()),
            heading: Some(heading.into()),
            cue: None,
            speaker: None,
            voice: None,
            direction: None,
            effect: None,
            text: None,
            cache: None,
            continuation: false,
        }
    }

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("radio_drama-music-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("music")).unwrap();
        dir
    }

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join("music").join(name), b"not really audio").unwrap();
    }

    #[test]
    fn a_scene_cue_runs_to_the_next_scene() {
        let dir = tmpdir("resolve");
        touch(&dir, "a.mp3");
        touch(&dir, "b.mp3");
        let entries = vec![
            scene("PROLOGUE \u{2014} TWO DESTINATIONS", 0.0),
            scene("SCENE ONE \u{2014} THE SIXFOLD MEMORY", 155.0),
        ];
        let m: MusicFile = toml::from_str(
            "[[cue]]\nfile = \"a.mp3\"\nscene = \"PROLOGUE\"\n\n[[cue]]\nfile = \"b.mp3\"\nscene = \"sixfold\"\n",
        )
        .unwrap();
        let got = m.resolve(&entries, 345.0, &dir).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].start_s, got[0].seconds), (0.0, 155.0));
        // The last scene runs to the end of the play.
        assert_eq!((got[1].start_s, got[1].seconds), (155.0, 190.0));
    }

    #[test]
    fn a_scene_that_matches_nothing_is_an_error_listing_the_scenes() {
        let dir = tmpdir("nomatch");
        touch(&dir, "a.mp3");
        let entries = vec![scene("PROLOGUE", 0.0)];
        let m: MusicFile =
            toml::from_str("[[cue]]\nfile = \"a.mp3\"\nscene = \"INTERVAL\"\n").unwrap();
        let err = m.resolve(&entries, 10.0, &dir).unwrap_err().to_string();
        assert!(
            err.contains("INTERVAL") && err.contains("PROLOGUE"),
            "{err}"
        );
    }

    #[test]
    fn an_ambiguous_scene_is_refused_rather_than_guessed() {
        let dir = tmpdir("ambiguous");
        touch(&dir, "a.mp3");
        let entries = vec![
            scene("SCENE FOUR \u{2014} THE ANGEL'S TEAR", 0.0),
            scene("SCENE FIVE \u{2014} THE ANGEL'S TEAR AGAIN", 60.0),
        ];
        let m: MusicFile =
            toml::from_str("[[cue]]\nfile = \"a.mp3\"\nscene = \"ANGEL'S TEAR\"\n").unwrap();
        let err = m.resolve(&entries, 120.0, &dir).unwrap_err().to_string();
        assert!(err.contains("matches 2 scenes"), "{err}");
    }

    #[test]
    fn a_missing_cue_file_is_caught_before_any_mixing() {
        let dir = tmpdir("missing");
        let entries = vec![scene("PROLOGUE", 0.0)];
        let m: MusicFile =
            toml::from_str("[[cue]]\nfile = \"gone.mp3\"\nscene = \"PROLOGUE\"\n").unwrap();
        let err = m.resolve(&entries, 10.0, &dir).unwrap_err().to_string();
        assert!(
            err.contains("gone.mp3") && err.contains("does not exist"),
            "{err}"
        );
    }

    #[test]
    fn an_explicit_time_overrides_the_scene_lookup() {
        let dir = tmpdir("at");
        touch(&dir, "a.mp3");
        let entries = vec![scene("PROLOGUE", 0.0)];
        let m: MusicFile = toml::from_str(
            "[[cue]]\nfile = \"a.mp3\"\nat_s = 12.5\nfor_s = 4.0\nonce = true\nno_duck = true\n",
        )
        .unwrap();
        let got = m.resolve(&entries, 100.0, &dir).unwrap();
        assert_eq!((got[0].start_s, got[0].seconds), (12.5, 4.0));
        assert!(got[0].once && got[0].no_duck);
    }

    #[test]
    fn a_cue_with_no_placement_at_all_is_an_error() {
        let dir = tmpdir("noplace");
        touch(&dir, "a.mp3");
        let m: MusicFile = toml::from_str("[[cue]]\nfile = \"a.mp3\"\n").unwrap();
        let err = m.resolve(&[], 10.0, &dir).unwrap_err().to_string();
        assert!(err.contains("neither a scene nor an at_s"), "{err}");
    }

    /// A short cue filling a long scene: the result is the right length,
    /// has no silent gaps at the loop points, and does not jump in level
    /// where one pass crosses into the next.
    #[test]
    fn a_short_cue_loops_to_fill_a_long_span() {
        let rate = 32_000usize;
        let len = rate; // a one-second cue
        let mut source = Stereo::silence(len);
        for i in 0..len {
            let v = (i as f32 / rate as f32 * 220.0 * std::f32::consts::TAU).sin() * 0.5;
            source.left[i] = v;
            source.right[i] = v;
        }
        let want = rate * 5;
        let (bed, passes) = fill(&source, want, rate / 4, false);
        assert_eq!(bed.frames(), want);
        assert!(passes >= 5, "{passes}");

        // No hole anywhere: every 50 ms window carries signal.
        let win = rate / 20;
        for w in bed.left.chunks(win) {
            let peak = w.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak > 0.05, "a gap at the loop point, peak {peak}");
        }
    }

    #[test]
    fn once_plays_a_cue_a_single_time() {
        let mut source = Stereo::silence(1000);
        source.left.iter_mut().for_each(|s| *s = 0.5);
        source.right.iter_mut().for_each(|s| *s = 0.5);
        let (bed, passes) = fill(&source, 5000, 100, true);
        assert_eq!(passes, 1);
        assert_eq!(bed.frames(), 5000);
        // Silent after the single pass.
        assert!(bed.left[2000..].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn ducking_pulls_down_under_speech_and_comes_back_after() {
        let rate = 32_000u32;
        // A second of silence, a second of speech, then five seconds of
        // silence: a 900 ms release is three time constants from being
        // fully back, so a shorter tail would be testing the exponential
        // rather than the ducker.
        let n = rate as usize * 7;
        let mut dialogue = vec![0.0f32; n];
        dialogue[rate as usize..2 * rate as usize].fill(0.4);
        let env = duck(&dialogue, rate, -40.0, -12.0, 80, 900);
        assert_eq!(env.len(), n);

        let ducked = dsp::db_to_gain(-12.0);
        // Before: open.
        assert!((env[rate as usize / 2] - 1.0).abs() < 1e-3);
        // By the end of the speech: down, close to the target.
        let at_end = env[2 * rate as usize - 1];
        assert!((at_end - ducked).abs() < 0.05, "{at_end} vs {ducked}");
        // Just after speech stops it is still down -- release is slow on
        // purpose, so music does not surge between two lines.
        assert!(env[2 * rate as usize + rate as usize / 20] < ducked + 0.1);
        // One second later it is only part of the way back, which is
        // what "slow release" means and what keeps the music from
        // pumping in the gaps between lines.
        let after_1s = env[3 * rate as usize];
        assert!((0.6..0.85).contains(&after_1s), "{after_1s}");
        // And by the end it has recovered.
        assert!(env[n - 1] > 0.95, "{}", env[n - 1]);
        // The envelope never exceeds unity or drops below the target.
        assert!(env.iter().all(|g| *g <= 1.0 + 1e-6 && *g >= ducked - 1e-6));
    }

    #[test]
    fn the_duck_attack_is_faster_than_its_release() {
        let rate = 32_000u32;
        let n = rate as usize * 4;
        let mut dialogue = vec![0.0f32; n];
        dialogue[rate as usize..2 * rate as usize].fill(0.4);
        let env = duck(&dialogue, rate, -40.0, -12.0, 80, 900);
        let ducked = dsp::db_to_gain(-12.0);
        let halfway = (1.0 + ducked) / 2.0;
        // The fall is measured from where speech starts, the rise from
        // where it stops.
        let fall = env[rate as usize..]
            .iter()
            .position(|g| *g < halfway)
            .unwrap();
        let rise = env[2 * rate as usize..]
            .iter()
            .position(|g| *g > halfway)
            .unwrap();
        assert!(rise > fall * 4, "fall {fall} rise {rise}");
    }

    #[test]
    fn the_scaffold_matches_cue_names_to_scene_titles() {
        let entries = vec![
            scene("PROLOGUE \u{2014} TWO DESTINATIONS", 0.0),
            scene("SCENE ONE \u{2014} THE SIXFOLD MEMORY", 100.0),
            scene("SCENE THREE \u{2014} THE ADMINISTRATUM", 200.0),
            scene("SCENE TWELVE \u{2014} THE SERVITOR WHO SERVES", 300.0),
        ];
        let cues: Vec<PathBuf> = [
            "M02_sixfold_memory.mp3",
            "M04_administratum.mp3",
            "M99_odd.mp3",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        let text = scaffold(&entries, &cues, Path::new("music"));
        let parsed: MusicFile = toml::from_str(&text).expect("scaffold is valid toml");
        assert_eq!(parsed.cues.len(), 2);
        let by_file: std::collections::HashMap<String, String> = parsed
            .cues
            .iter()
            .map(|c| {
                (
                    c.file.display().to_string(),
                    c.scene.clone().unwrap_or_default(),
                )
            })
            .collect();
        assert!(by_file["M02_sixfold_memory.mp3"].contains("SIXFOLD"));
        assert!(by_file["M04_administratum.mp3"].contains("ADMINISTRATUM"));
        // The scenes and cues it could not pair are both written down.
        assert!(text.contains("# no cue matched: PROLOGUE"), "{text}");
        assert!(text.contains("# no cue matched: SCENE TWELVE"), "{text}");
        assert!(text.contains("M99_odd.mp3"));
    }

    /// "Prologue" and "Epilogue" read like structural filler next to
    /// "Scene", but they are the only name those parts have.
    #[test]
    fn a_cue_named_only_for_its_structural_part_still_matches() {
        let entries = vec![
            scene("PROLOGUE \u{2014} TWO DESTINATIONS", 0.0),
            scene("EPILOGUE \u{2014} THE GAME HAS NOT YET BEGUN", 100.0),
            scene("POST-CREDITS STINGER \u{2014} A PRIVATE DIAGNOSTIC", 200.0),
        ];
        let cues: Vec<PathBuf> = ["M13_epilogue.mp3", "M14_stinger.mp3", "M00_prologue.mp3"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let text = scaffold(&entries, &cues, Path::new("music"));
        let parsed: MusicFile = toml::from_str(&text).unwrap();
        assert_eq!(parsed.cues.len(), 3, "{text}");
        let placed: Vec<(String, String)> = parsed
            .cues
            .iter()
            .map(|c| {
                (
                    c.file.display().to_string(),
                    c.scene.clone().unwrap_or_default(),
                )
            })
            .collect();
        for (file, scene) in &placed {
            let want = match file.as_str() {
                "M13_epilogue.mp3" => "EPILOGUE",
                "M14_stinger.mp3" => "STINGER",
                _ => "PROLOGUE",
            };
            assert!(scene.contains(want), "{file} went to {scene}");
        }
    }

    #[test]
    fn the_scaffold_does_not_give_two_cues_the_same_scene() {
        let entries = vec![
            scene("SCENE ONE \u{2014} THE ADMINISTRATUM", 0.0),
            scene("SCENE TWO \u{2014} THE ARCHIVE", 100.0),
        ];
        let cues: Vec<PathBuf> = ["M01_administratum.mp3", "M02_administratum_reprise.mp3"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let text = scaffold(&entries, &cues, Path::new("music"));
        let parsed: MusicFile = toml::from_str(&text).unwrap();
        let scenes: Vec<_> = parsed.cues.iter().filter_map(|c| c.scene.clone()).collect();
        assert_eq!(
            scenes.len(),
            scenes
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
    }
}

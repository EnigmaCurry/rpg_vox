//! Turning a parsed script and a resolved cast into a finished recording.
//!
//! Three passes, in this order, because each one wants to be able to fail
//! before the next has cost anything:
//!
//! 1. **Plan.** Walk the script and work out every utterance that has to
//!    exist, every gap between them, and where each one lands. Nothing is
//!    synthesized and nothing is read; a plan is cheap enough to print
//!    (`radio_drama check`) and is what reports the play's length before
//!    a single request goes out.
//! 2. **Synthesize.** Fill the cache. Identical text in the same voice is
//!    one utterance however many times the script says it, requests go
//!    out `--jobs` at a time, and anything already cached is skipped — so
//!    the second render of a script with one edited line costs one
//!    request.
//! 3. **Mix.** Read the cache back, trim, apply the cast's levels and the
//!    directions' effects, and lay it all out on a timeline against the
//!    gaps the plan decided. Every effect lives in this pass, which is
//!    what makes re-pacing or re-balancing a play a seconds-long job
//!    rather than a re-render.
//!
//! The manifest written alongside the audio is the third output and the
//! reason the cues are kept: every `[SFX]`, `[MUSIC]` and `[TRANSITION]`
//! in the script is in it with the timestamps of the silence left for it,
//! next to every line with the timestamps of the speech. Laying music and
//! effects under this production later is then a matter of reading that
//! file, not of parsing the script again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tracing::{debug, info, warn};

use crate::audio;
use crate::cast::{DirectionEffect, ResolvedCast, VoiceSource};
use crate::dsp;
use crate::qwen3;
use crate::script::{Element, Script};

/// Bumped when a change to this crate would make a cached utterance the
/// wrong answer for its key. Nothing about the *mix* belongs in the key —
/// levels, filters, trimming and pacing are all applied to the cache
/// rather than baked into it — so in practice this changes only if what
/// gets sent to the server does.
const CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Flac,
    Wav,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Flac => "flac",
            Format::Wav => "wav",
        }
    }
}

/// One distinct thing that has to be synthesized.
#[derive(Debug, Clone)]
pub struct Utterance {
    /// The voice's name on the server, which is also what makes this
    /// utterance distinct from the same words said by somebody else.
    pub voice_id: String,
    pub language: String,
    pub text: String,
    /// Hex sha256 of what identifies this utterance; also its cache
    /// filename.
    pub key: String,
    /// How many places in the play say it.
    pub uses: usize,
}

/// What one element of the script contributes to the timeline.
#[derive(Debug, Clone)]
pub struct Step {
    /// Index into the script's elements.
    pub element: usize,
    /// Silence before this element's own content. Only a line has any:
    /// the pacing between two lines belongs to the second of them, while
    /// a cue or a pause *is* its silence and needs none in front.
    pub lead_s: f32,
    pub what: StepKind,
}

#[derive(Debug, Clone)]
pub enum StepKind {
    /// A gap of a known length: a scene break, a slug, a pause, or the
    /// hole left where an effect will go.
    Silence { seconds: f32 },
    /// An utterance, with the direction effect to apply to it.
    Speech {
        utterance: usize,
        effect: Option<(String, DirectionEffect)>,
        gain_db: f32,
    },
}

/// A whole render, decided but not performed.
#[derive(Debug)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub utterances: Vec<Utterance>,
    /// Directions in the script that no effect matched. Not an error —
    /// "with ritual gravity" is a note to a human performer and there is
    /// nothing honest to do with it — but worth saying out loud.
    pub unmatched_directions: Vec<String>,
}

impl Plan {
    pub fn build(script: &Script, cast: &ResolvedCast) -> Result<Self> {
        if !cast.uncast.is_empty() {
            // The caller is expected to have reported these; refusing
            // here as well means no code path can quietly render a play
            // with a character missing.
            bail!(
                "{} character(s) have no voice: {}",
                cast.uncast.len(),
                cast.uncast
                    .iter()
                    .map(|s| s.display.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        let mut steps = Vec::new();
        let mut utterances: Vec<Utterance> = Vec::new();
        let mut by_key: HashMap<String, usize> = HashMap::new();
        let mut unmatched: Vec<String> = Vec::new();
        let p = &cast.production;

        // The previous element, which is all the pacing rules need: a
        // gap between two lines depends on whether the speaker changed
        // and whether the script wrote a new label, and a gap after
        // anything else has already been accounted for by that thing.
        let mut prev: Option<&Element> = None;

        for (i, element) in script.elements.iter().enumerate() {
            let (lead_s, what) = match element {
                Element::Scene(_) => (0.0, StepKind::Silence { seconds: p.scene_s }),
                Element::Slug(_) => (0.0, StepKind::Silence { seconds: p.slug_s }),
                Element::Pause { long } => (
                    0.0,
                    StepKind::Silence {
                        seconds: if *long { p.long_pause_s } else { p.pause_s },
                    },
                ),
                Element::Cue { .. } => (0.0, StepKind::Silence { seconds: p.cue_s }),
                Element::Line(line) => {
                    let lead = match prev {
                        Some(Element::Line(before)) => {
                            if before.key != line.key {
                                p.speaker_s
                            } else if line.continuation {
                                p.paragraph_s
                            } else {
                                p.line_s
                            }
                        }
                        _ => 0.0,
                    };
                    let voice = cast.voice_for(&line.key).with_context(|| {
                        format!("no voice for {} after resolving the cast", line.speaker)
                    })?;
                    let key = cache_key(&voice.id, &voice.language, &line.text);
                    let utterance = match by_key.get(&key) {
                        Some(&idx) => {
                            utterances[idx].uses += 1;
                            idx
                        }
                        None => {
                            utterances.push(Utterance {
                                voice_id: voice.id.clone(),
                                language: voice.language.clone(),
                                text: line.text.clone(),
                                key: key.clone(),
                                uses: 1,
                            });
                            by_key.insert(key, utterances.len() - 1);
                            utterances.len() - 1
                        }
                    };
                    let effect = match line.direction.as_deref() {
                        Some(direction) => match cast.effect_for(direction) {
                            Some((matched, eff)) => Some((matched.to_string(), eff.clone())),
                            None => {
                                if !unmatched.iter().any(|d| d == direction) {
                                    unmatched.push(direction.to_string());
                                }
                                None
                            }
                        },
                        None => None,
                    };
                    (
                        lead,
                        StepKind::Speech {
                            utterance,
                            effect,
                            gain_db: voice.gain_db,
                        },
                    )
                }
            };
            steps.push(Step {
                element: i,
                lead_s,
                what,
            });
            prev = Some(element);
        }

        Ok(Self {
            steps,
            utterances,
            unmatched_directions: unmatched,
        })
    }

    /// Length of the silence this plan already knows about. The speech is
    /// however long the synthesizer makes it, so this is a floor on the
    /// finished play rather than its duration.
    pub fn silence_s(&self) -> f32 {
        self.steps
            .iter()
            .map(|s| {
                s.lead_s
                    + match &s.what {
                        StepKind::Silence { seconds } => *seconds,
                        StepKind::Speech { .. } => 0.0,
                    }
            })
            .sum()
    }

    pub fn speech_steps(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| matches!(s.what, StepKind::Speech { .. }))
            .count()
    }
}

/// What identifies an utterance, and so what a change to has to produce
/// a different cache entry: the voice it is said in, the language the
/// model is told it is, and the words.
fn cache_key(voice_id: &str, language: &str, text: &str) -> String {
    let mut h = Sha256::new();
    h.update(format!("radio_drama/utterance/{CACHE_VERSION}\n").as_bytes());
    h.update(format!("voice\t{voice_id}\n").as_bytes());
    h.update(format!("language\t{language}\n").as_bytes());
    h.update(format!("text\t{text}\n").as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SynthStats {
    pub cached: usize,
    pub synthesized: usize,
}

/// Make sure every voice the plan needs exists on the server.
///
/// Registered-by-name voices are checked rather than uploaded, because
/// nothing here has the audio for them; a name that is not there is an
/// error now instead of four hundred failed synth requests later.
pub async fn ensure_voices(client: &qwen3::Client, cast: &ResolvedCast) -> Result<usize> {
    let present = client
        .voices()
        .await
        .with_context(|| format!("listing voices on {}", client.base()))?;
    let mut uploaded = 0;

    for voice in &cast.voices {
        if present.iter().any(|v| v == &voice.id) {
            debug!(voice = %voice.id, "voice already registered");
            continue;
        }
        match &voice.source {
            VoiceSource::Registered => bail!(
                "{} is cast as server_voice {:?}, which {} does not have. It has: {}",
                voice.characters.join(", "),
                voice.id,
                client.base(),
                if present.is_empty() {
                    "nothing".to_string()
                } else {
                    present.join(", ")
                }
            ),
            VoiceSource::Sample { path, ref_text } => {
                let wav = audio::reference_as_wav(path)?;
                info!(
                    voice = %voice.id,
                    characters = %voice.characters.join(", "),
                    sample = %path.display(),
                    "registering a new voice"
                );
                client.register(&voice.id, wav, ref_text).await?;
                uploaded += 1;
            }
        }
    }
    Ok(uploaded)
}

/// Fill the cache. Returns what it had to do.
pub async fn synthesize(
    client: &qwen3::Client,
    plan: &Plan,
    cache_dir: &Path,
    jobs: usize,
    refresh: bool,
) -> Result<SynthStats> {
    std::fs::create_dir_all(cache_dir)
        .with_context(|| format!("creating cache directory {}", cache_dir.display()))?;

    let wanted: Vec<&Utterance> = plan
        .utterances
        .iter()
        .filter(|u| refresh || !cache_path(cache_dir, &u.key).exists())
        .collect();
    let stats = SynthStats {
        cached: plan.utterances.len() - wanted.len(),
        synthesized: wanted.len(),
    };
    if wanted.is_empty() {
        return Ok(stats);
    }

    let total = wanted.len();
    let done = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(tokio::sync::Semaphore::new(jobs.max(1)));
    let mut tasks = Vec::with_capacity(total);

    for utterance in wanted {
        let client = client.clone();
        let gate = Arc::clone(&gate);
        let done = Arc::clone(&done);
        let path = cache_path(cache_dir, &utterance.key);
        let (voice, language, text) = (
            utterance.voice_id.clone(),
            utterance.language.clone(),
            utterance.text.clone(),
        );
        tasks.push(tokio::spawn(async move {
            let _permit = gate
                .acquire()
                .await
                .expect("synthesis gate outlives its tasks");
            let wav = client
                .speak(&voice, &language, &text)
                .await
                .with_context(|| format!("synthesizing {:?} in {voice}", snippet(&text)))?;
            audio::write_atomic(&path, &wav.0)?;
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            eprint!("\r  synthesizing {n}/{total}   ");
            Ok::<(), anyhow::Error>(())
        }));
    }

    let mut failures = Vec::new();
    for task in tasks {
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => failures.push(format!("{e:#}")),
            Err(e) => failures.push(format!("synthesis task panicked: {e}")),
        }
    }
    eprintln!();
    if !failures.is_empty() {
        // Every failure, not just the first: when a server is refusing
        // requests the shape of the refusals is the diagnosis.
        bail!(
            "{} of {total} utterances failed:\n  {}",
            failures.len(),
            failures.join("\n  ")
        );
    }
    Ok(stats)
}

pub fn cache_path(cache_dir: &Path, key: &str) -> PathBuf {
    cache_dir.join(format!("{key}.wav"))
}

fn snippet(text: &str) -> String {
    let mut s: String = text.chars().take(60).collect();
    if text.chars().count() > 60 {
        s.push('…');
    }
    s
}

/// The finished mix and everything there is to say about it.
#[derive(Debug)]
pub struct Mix {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub entries: Vec<ManifestEntry>,
    /// Gain the peak limiter had to apply, in dB. Zero when nothing
    /// needed pulling down.
    pub limited_db: f32,
    pub peak_db: Option<f32>,
    /// Lines that came back far longer than their words can account
    /// for. See [`overlong_lines`].
    pub overlong: Vec<Overlong>,
}

/// A line whose audio is much longer than its text should take to say.
///
/// Synthesis is sampled, so once in a few hundred utterances the model
/// does not stop when it has finished the words and fills the rest of
/// its budget with something else. One such line in a forty-minute
/// drama is fifteen seconds of nonsense that nobody will find by
/// looking, which is reason enough to point at it: re-synthesizing is
/// one request, because deleting its cache entry is all it takes.
#[derive(Debug, Clone)]
pub struct Overlong {
    pub speaker: String,
    pub text: String,
    pub cache: String,
    pub start_s: f64,
    pub seconds: f64,
    /// What it should have taken at the play's own median pace.
    pub expected_s: f64,
}

/// How many times longer than the median pace a line has to be before
/// it is called out. Measured against a 783-line render whose slowest
/// legitimate narration sat at about half the median rate and whose one
/// genuinely broken line sat at a quarter of it, so this is set between
/// the two rather than at a round number.
const OVERLONG_FACTOR: f64 = 2.5;

/// Find the lines worth a second look.
///
/// The yardstick is the play's own median speaking rate rather than a
/// fixed words-per-second, so a deliberately slow cast is not reported
/// wholesale. Short lines are skipped: a three-word line is mostly
/// trim margins and breath, and its rate says little.
fn overlong_lines(entries: &[ManifestEntry]) -> Vec<Overlong> {
    let measurable: Vec<(f64, &ManifestEntry)> = entries
        .iter()
        .filter(|e| e.kind == "line")
        .filter_map(|e| {
            let words = e
                .text
                .as_deref()
                .map_or(0, |t| t.split_whitespace().count());
            let seconds = e.end_s - e.start_s;
            (words >= 4 && seconds > 0.0).then_some((words as f64 / seconds, e))
        })
        .collect();
    if measurable.len() < 20 {
        // Too few to have a median worth comparing against.
        return Vec::new();
    }
    let mut rates: Vec<f64> = measurable.iter().map(|(r, _)| *r).collect();
    rates.sort_by(|a, b| a.partial_cmp(b).expect("rates are finite"));
    let median = rates[rates.len() / 2];

    let mut out: Vec<Overlong> = measurable
        .iter()
        .filter_map(|(rate, e)| {
            let seconds = e.end_s - e.start_s;
            let expected_s = e
                .text
                .as_deref()
                .map_or(0, |t| t.split_whitespace().count()) as f64
                / median;
            // Both tests: far off the pace, and long enough in absolute
            // terms that somebody would hear it as a hole.
            (*rate * OVERLONG_FACTOR < median && seconds > 3.0).then(|| Overlong {
                speaker: e.speaker.clone().unwrap_or_default(),
                text: e.text.clone().unwrap_or_default(),
                cache: e.cache.clone().unwrap_or_default(),
                start_s: e.start_s,
                seconds,
                expected_s,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        (b.seconds / b.expected_s.max(0.001))
            .partial_cmp(&(a.seconds / a.expected_s.max(0.001)))
            .expect("ratios are finite")
    });
    out
}

impl Mix {
    pub fn duration_s(&self) -> f64 {
        self.samples.len() as f64 / self.sample_rate as f64
    }
}

/// Lay the cached utterances out on a timeline with the plan's gaps
/// between them.
pub fn mix(
    script: &Script,
    cast: &ResolvedCast,
    plan: &Plan,
    cache_dir: &Path,
    peak_target_db: f32,
) -> Result<Mix> {
    let p = &cast.production;
    // Read each utterance once however many times the play says it, and
    // keep it post-trim: the trim is the same wherever it lands, and the
    // effects are not, so this is the furthest the work can be shared.
    let mut loaded: HashMap<usize, (Vec<f32>, u32)> = HashMap::new();
    let mut rate: Option<u32> = None;

    for (idx, utterance) in plan.utterances.iter().enumerate() {
        let path = cache_path(cache_dir, &utterance.key);
        let pcm = audio::read_wav(&path).with_context(|| {
            format!(
                "reading cached audio for {:?} ({})",
                snippet(&utterance.text),
                path.display()
            )
        })?;
        match rate {
            None => rate = Some(pcm.sample_rate),
            Some(r) if r != pcm.sample_rate => bail!(
                "cached utterances disagree about sample rate ({r} Hz and {} Hz). \
                 The cache was written against a different server; re-render with --refresh.",
                pcm.sample_rate
            ),
            Some(_) => {}
        }
        let keep = (p.trim_keep_ms as f32 * pcm.sample_rate as f32 / 1000.0) as usize;
        let (start, end) = dsp::trim_silence(&pcm.samples, p.trim_db, keep, pcm.sample_rate);
        let mut samples = pcm.samples[start..end].to_vec();
        dsp::fade_edges(
            &mut samples,
            (p.fade_ms as f32 * pcm.sample_rate as f32 / 1000.0) as usize,
        );
        if samples.is_empty() {
            warn!(text = %snippet(&utterance.text), "cached utterance is empty");
        }
        loaded.insert(idx, (samples, pcm.sample_rate));
    }

    // A play with no lines at all still has a rate to write its silence
    // at; the synthesizer's is the only one in the pipeline.
    let rate = rate.unwrap_or(24_000);
    let samples_for = |seconds: f32| -> usize { (seconds.max(0.0) * rate as f32).round() as usize };

    let mut out: Vec<f32> = Vec::new();
    let mut entries = Vec::new();
    let mut scene: Option<String> = None;

    for step in &plan.steps {
        out.extend(std::iter::repeat_n(0.0, samples_for(step.lead_s)));
        let start = out.len();
        let element = &script.elements[step.element];

        if let Element::Scene(title) = element {
            scene = Some(title.clone());
        }

        match &step.what {
            StepKind::Silence { seconds } => {
                out.extend(std::iter::repeat_n(0.0, samples_for(*seconds)));
            }
            StepKind::Speech {
                utterance,
                effect,
                gain_db,
            } => {
                let (cached, _) = &loaded[utterance];
                let mut voiced = cached.clone();
                if let Some((_, eff)) = effect {
                    dsp::apply_effect(&mut voiced, eff, rate);
                }
                dsp::apply_gain(&mut voiced, *gain_db);
                out.extend_from_slice(&voiced);
            }
        }

        let end = out.len();
        entries.push(manifest_entry(
            element,
            &plan.utterances,
            &step.what,
            scene.as_deref(),
            start as f64 / rate as f64,
            end as f64 / rate as f64,
        ));
    }

    let limited_db = dsp::limit_peak(&mut out, peak_target_db);
    let peak_db = dsp::peak_db(&out);
    let overlong = overlong_lines(&entries);
    Ok(Mix {
        samples: out,
        sample_rate: rate,
        entries,
        limited_db,
        peak_db,
        overlong,
    })
}

/// One row of the manifest. Flat and string-tagged rather than a tagged
/// union, because the thing most likely to read it is a few lines of
/// `jq` looking for every cue with its timestamps.
/// Deserialize as well as Serialize: `radio_drama music` reads back the
/// manifest a render wrote, because where a scene *is* in the finished
/// audio is a fact about the render, not about the script.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// `"scene"`, `"slug"`, `"line"`, `"pause"`, `"long_pause"`,
    /// `"cue"`. A `String` rather than a `&'static str` so the manifest
    /// reads back as well as writes: `radio_drama music` needs the
    /// scenes' timings out of it.
    pub kind: String,
    pub start_s: f64,
    pub end_s: f64,
    /// The scene this falls in, including for the scene heading itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
    /// A scene's or slug's text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    /// `SFX`, `MUSIC`, `TRANSITION`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// Which `[directions]` key the direction was honoured as, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
    /// A cue's description, or what a line says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The cache entry this line's audio came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<String>,
    /// `default` as well as `skip_serializing_if`: the field is left out
    /// when false, so reading a manifest back has to tolerate its
    /// absence.
    #[serde(default, skip_serializing_if = "is_false")]
    pub continuation: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn manifest_entry(
    element: &Element,
    utterances: &[Utterance],
    what: &StepKind,
    scene: Option<&str>,
    start_s: f64,
    end_s: f64,
) -> ManifestEntry {
    let base = ManifestEntry {
        kind: String::new(),
        start_s,
        end_s,
        scene: scene.map(str::to_string),
        heading: None,
        cue: None,
        speaker: None,
        voice: None,
        direction: None,
        effect: None,
        text: None,
        cache: None,
        continuation: false,
    };
    match element {
        Element::Scene(title) => ManifestEntry {
            kind: "scene".into(),
            heading: Some(title.clone()),
            ..base
        },
        Element::Slug(text) => ManifestEntry {
            kind: "slug".into(),
            heading: Some(text.clone()),
            ..base
        },
        Element::Pause { long } => ManifestEntry {
            kind: if *long { "long_pause" } else { "pause" }.into(),
            ..base
        },
        Element::Cue { kind, text } => ManifestEntry {
            kind: "cue".into(),
            cue: Some(kind.clone()),
            text: (!text.is_empty()).then(|| text.clone()),
            ..base
        },
        Element::Line(line) => {
            let (voice, cache, effect) = match what {
                StepKind::Speech {
                    utterance, effect, ..
                } => (
                    Some(utterances[*utterance].voice_id.clone()),
                    Some(utterances[*utterance].key.clone()),
                    effect.as_ref().map(|(k, _)| k.clone()),
                ),
                StepKind::Silence { .. } => (None, None, None),
            };
            ManifestEntry {
                kind: "line".into(),
                speaker: Some(line.speaker.clone()),
                voice,
                direction: line.direction.clone(),
                effect,
                text: Some(line.text.clone()),
                cache,
                continuation: line.continuation,
                ..base
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestVoice {
    pub id: String,
    pub language: String,
    pub characters: Vec<String>,
    pub gain_db: f32,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Manifest {
    pub script: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    pub audio: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub duration_s: f64,
    pub generated_utc: String,
    pub voices: Vec<ManifestVoice>,
    /// The score, when one was laid under this render.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub music: Vec<ManifestCue>,
    pub entries: Vec<ManifestEntry>,
}

/// One cue as it ended up: where it sits in the finished audio, and
/// what it took to fill that span.
#[derive(Debug, Clone, Serialize)]
pub struct ManifestCue {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
    pub start_s: f64,
    pub end_s: f64,
    pub gain_db: f32,
    pub passes: usize,
    #[serde(skip_serializing_if = "is_false")]
    pub no_duck: bool,
}

impl Manifest {
    pub fn build(
        script_path: &Path,
        script: &Script,
        cast: &ResolvedCast,
        mix: &Mix,
        audio_name: &str,
        scored: Option<&crate::music::Scored>,
    ) -> Self {
        Self {
            script: script_path.display().to_string(),
            title: cast
                .production
                .title
                .clone()
                .or_else(|| script.title.clone()),
            subtitle: script.subtitle.clone(),
            audio: audio_name.to_string(),
            sample_rate: scored.map_or(mix.sample_rate, |s| s.sample_rate),
            channels: if scored.is_some() { 2 } else { 1 },
            duration_s: scored.map_or_else(|| mix.duration_s(), |s| s.duration_s()),
            generated_utc: chrono::Utc::now().to_rfc3339(),
            voices: cast
                .voices
                .iter()
                .map(|v| ManifestVoice {
                    id: v.id.clone(),
                    language: v.language.clone(),
                    characters: v.characters.clone(),
                    gain_db: v.gain_db,
                    source: match &v.source {
                        VoiceSource::Registered => "server".into(),
                        VoiceSource::Sample { path, .. } => path.display().to_string(),
                    },
                })
                .collect(),
            music: scored.map_or_else(Vec::new, |s| {
                s.placements
                    .iter()
                    .map(|p| ManifestCue {
                        file: p
                            .file
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        scene: p.scene.clone(),
                        start_s: p.start_s,
                        end_s: p.start_s + p.seconds,
                        gain_db: p.gain_db,
                        passes: p.loops,
                        no_duck: p.no_duck,
                    })
                    .collect()
            }),
            entries: mix.entries.clone(),
        }
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self).context("serializing manifest")?;
        audio::write_atomic(path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cast::CastFile;
    use crate::script::ParseOptions;

    fn setup(md: &str, cast_toml: &str) -> (Script, ResolvedCast) {
        let script = Script::parse(md, &ParseOptions::default());
        let cast_file: CastFile = toml::from_str(cast_toml).expect("cast parses");
        let cast = cast_file
            .resolve(&script, Path::new("."))
            .expect("cast resolves");
        (script, cast)
    }

    const ONE_VOICE: &str =
        "[cast.MAX]\nserver_voice = \"m\"\n\n[cast.VOSS]\nserver_voice = \"v\"\n";

    #[test]
    fn identical_lines_in_one_voice_are_synthesized_once() {
        let (s, c) = setup(
            "**MAX:** Maybe.\n\n**VOSS:** Yes.\n\n**MAX:** Maybe.\n",
            ONE_VOICE,
        );
        let plan = Plan::build(&s, &c).unwrap();
        assert_eq!(plan.speech_steps(), 3);
        assert_eq!(plan.utterances.len(), 2);
        let maybe = plan.utterances.iter().find(|u| u.text == "Maybe.").unwrap();
        assert_eq!(maybe.uses, 2);
    }

    #[test]
    fn the_same_words_in_two_voices_are_two_utterances() {
        let (s, c) = setup("**MAX:** Maybe.\n\n**VOSS:** Maybe.\n", ONE_VOICE);
        let plan = Plan::build(&s, &c).unwrap();
        assert_eq!(plan.utterances.len(), 2);
    }

    #[test]
    fn a_cache_key_turns_on_voice_language_and_text_only() {
        let a = cache_key("v", "English", "Yes.");
        assert_eq!(a, cache_key("v", "English", "Yes."));
        assert_ne!(a, cache_key("w", "English", "Yes."));
        assert_ne!(a, cache_key("v", "French", "Yes."));
        assert_ne!(a, cache_key("v", "English", "Yes!"));
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn pacing_depends_on_what_came_before() {
        let (s, c) = setup(
            "**MAX:** One.\n\nTwo.\n\n**MAX:** Three.\n\n**VOSS:** Four.\n\n[PAUSE.]\n\n**VOSS:** Five.\n",
            ONE_VOICE,
        );
        let plan = Plan::build(&s, &c).unwrap();
        let leads: Vec<f32> = plan.steps.iter().map(|st| st.lead_s).collect();
        let p = &c.production;
        assert_eq!(
            leads,
            vec![
                0.0,           // the first line
                p.paragraph_s, // a continuation of the same speech
                p.line_s,      // the same speaker, a new speech
                p.speaker_s,   // a different speaker
                0.0,           // a pause needs no lead of its own
                0.0,           // and the line after it inherits none
            ]
        );
    }

    #[test]
    fn silence_adds_up_to_what_the_plan_knows() {
        let (s, c) = setup(
            "## ONE\n\n**INT. ANYWHERE**\n\n[SFX: Bells.]\n\n**MAX:** Hello.\n\n[LONG PAUSE.]\n",
            ONE_VOICE,
        );
        let plan = Plan::build(&s, &c).unwrap();
        let p = &c.production;
        let want = p.scene_s + p.slug_s + p.cue_s + p.long_pause_s;
        assert!(
            (plan.silence_s() - want).abs() < 1e-5,
            "{}",
            plan.silence_s()
        );
    }

    #[test]
    fn an_uncast_character_stops_the_plan() {
        let s = Script::parse(
            "**MAX:** One.\n\n**VOSS:** Two.\n",
            &ParseOptions::default(),
        );
        let cast_file: CastFile = toml::from_str("[cast.MAX]\nserver_voice = \"m\"\n").unwrap();
        let cast = cast_file.resolve(&s, Path::new(".")).unwrap();
        let err = Plan::build(&s, &cast).unwrap_err().to_string();
        assert!(err.contains("VOSS"), "{err}");
    }

    #[test]
    fn a_direction_with_no_effect_is_reported_not_dropped() {
        let (s, c) = setup(
            "**MAX** *(quietly)*: One.\n\n**VOSS** *(with ritual gravity)*: Two.\n",
            ONE_VOICE,
        );
        let plan = Plan::build(&s, &c).unwrap();
        assert_eq!(plan.unmatched_directions, vec!["with ritual gravity"]);
        let honoured: Vec<String> = plan
            .steps
            .iter()
            .filter_map(|st| match &st.what {
                StepKind::Speech { effect, .. } => effect.as_ref().map(|(k, _)| k.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(honoured, vec!["quietly"]);
    }

    /// The whole mix pass against a cache written by hand, which is the
    /// only part of a render that does not need a server.
    #[test]
    fn mixing_lays_speech_and_silence_out_in_order() {
        let dir = std::env::temp_dir().join("radio_drama-test-mix");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (s, c) = setup(
            "## ONE\n\n**MAX:** Hello.\n\n[PAUSE.]\n\n**VOSS** *(quietly)*: Goodbye.\n",
            ONE_VOICE,
        );
        let plan = Plan::build(&s, &c).unwrap();
        let rate = 24_000;
        // A half-second of tone per utterance, loud enough to survive
        // the trim, with no padding to trim off.
        for u in &plan.utterances {
            let samples: Vec<f32> = (0..rate / 2)
                .map(|i| (i as f32 / rate as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.5)
                .collect();
            audio::write_wav(&cache_path(&dir, &u.key), &samples, rate as u32).unwrap();
        }

        let mix = mix(&s, &c, &plan, &dir, -1.0).unwrap();
        assert_eq!(mix.sample_rate, 24_000);

        let kinds: Vec<&str> = mix.entries.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["scene", "line", "pause", "line"]);

        // Every entry runs forward and none overlaps the next.
        for pair in mix.entries.windows(2) {
            assert!(pair[0].end_s <= pair[1].start_s + 1e-9, "{pair:?}");
        }
        let last = mix.entries.last().unwrap();
        assert!((last.end_s - mix.duration_s()).abs() < 1e-6);

        // The scene gap, then the first line; the pause then sits
        // between the two speeches at its full length.
        let scene = &mix.entries[0];
        assert!((scene.end_s - scene.start_s - 2.0).abs() < 1e-3);
        let pause = &mix.entries[2];
        assert!((pause.end_s - pause.start_s - 1.0).abs() < 1e-3);

        // The quiet line was attenuated relative to the other.
        let voss = &mix.entries[3];
        assert_eq!(voss.effect.as_deref(), Some("quietly"));
        assert_eq!(voss.speaker.as_deref(), Some("VOSS"));
        assert!(voss.cache.is_some());
        assert_eq!(voss.scene.as_deref(), Some("ONE"));

        let at = |e: &ManifestEntry| {
            let a = (e.start_s * rate as f64) as usize;
            let b = (e.end_s * rate as f64) as usize;
            dsp::peak_db(&mix.samples[a..b]).unwrap()
        };
        let loud = at(&mix.entries[1]);
        let quiet = at(voss);
        assert!(quiet < loud - 4.0, "loud {loud} quiet {quiet}");
    }

    /// A line of normal length, and the same words taking four times
    /// as long: only the second is a failed generation.
    /// `radio_drama music` reads the manifest a render wrote, so what
    /// is written has to be what can be read -- including the fields
    /// that are left out when they are empty.
    #[test]
    fn a_manifest_entry_round_trips_through_json() {
        for entry in [
            ManifestEntry {
                kind: "line".into(),
                start_s: 1.0,
                end_s: 2.5,
                scene: Some("SCENE ONE".into()),
                heading: None,
                cue: None,
                speaker: Some("VOSS".into()),
                voice: Some("v".into()),
                direction: Some("over vox".into()),
                effect: Some("over vox".into()),
                text: Some("Analysis continues.".into()),
                cache: Some("abc".into()),
                continuation: true,
            },
            // The sparse case: a scene heading, with every optional
            // field and the false bool all omitted on the way out.
            ManifestEntry {
                kind: "scene".into(),
                start_s: 0.0,
                end_s: 2.0,
                scene: Some("SCENE ONE".into()),
                heading: Some("SCENE ONE".into()),
                cue: None,
                speaker: None,
                voice: None,
                direction: None,
                effect: None,
                text: None,
                cache: None,
                continuation: false,
            },
        ] {
            let json = serde_json::to_string(&entry).unwrap();
            let back: ManifestEntry =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("{json}: {e}"));
            assert_eq!(back.kind, entry.kind);
            assert_eq!(back.start_s, entry.start_s);
            assert_eq!(back.heading, entry.heading);
            assert_eq!(back.speaker, entry.speaker);
            assert_eq!(back.cache, entry.cache);
            assert_eq!(back.continuation, entry.continuation);
        }
    }

    #[test]
    fn an_overlong_line_is_found_and_a_merely_slow_one_is_not() {
        fn line(text: &str, start: f64, seconds: f64) -> ManifestEntry {
            ManifestEntry {
                kind: "line".into(),
                start_s: start,
                end_s: start + seconds,
                scene: None,
                heading: None,
                cue: None,
                speaker: Some("VOSS".into()),
                voice: Some("v".into()),
                direction: None,
                effect: None,
                text: Some(text.into()),
                cache: Some(format!("cache-{start}")),
                continuation: false,
            }
        }
        // Thirty lines at five words in two seconds: 2.5 words/second.
        let mut entries: Vec<ManifestEntry> = (0..30)
            .map(|i| line("one two three four five", i as f64 * 10.0, 2.0))
            .collect();
        // Slower than the pace, but not by enough to be broken.
        entries.push(line("one two three four five", 400.0, 3.5));
        // The same five words over fifteen seconds.
        entries.push(line("one two three four five", 500.0, 15.0));

        let found = overlong_lines(&entries);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].start_s, 500.0);
        assert!((found[0].seconds - 15.0).abs() < 1e-9);
        assert!(
            (found[0].expected_s - 2.0).abs() < 0.1,
            "{}",
            found[0].expected_s
        );
        assert_eq!(found[0].cache, "cache-500");
    }

    #[test]
    fn a_short_line_is_not_judged_on_its_rate() {
        // Mostly trim margin and breath; its rate means nothing.
        let mut entries: Vec<ManifestEntry> = Vec::new();
        for i in 0..30 {
            entries.push(ManifestEntry {
                kind: "line".into(),
                start_s: i as f64 * 10.0,
                end_s: i as f64 * 10.0 + 2.0,
                scene: None,
                heading: None,
                cue: None,
                speaker: Some("A".into()),
                voice: Some("v".into()),
                direction: None,
                effect: None,
                text: Some("one two three four five".into()),
                cache: Some("c".into()),
                continuation: false,
            });
        }
        let mut short = entries[0].clone();
        short.start_s = 400.0;
        short.end_s = 410.0;
        short.text = Some("Yes.".into());
        entries.push(short);
        assert!(overlong_lines(&entries).is_empty());
    }

    #[test]
    fn too_few_lines_to_have_a_pace_reports_nothing() {
        let entries: Vec<ManifestEntry> = (0..5)
            .map(|i| ManifestEntry {
                kind: "line".into(),
                start_s: i as f64,
                end_s: i as f64 + 20.0,
                scene: None,
                heading: None,
                cue: None,
                speaker: Some("A".into()),
                voice: Some("v".into()),
                direction: None,
                effect: None,
                text: Some("one two three four five".into()),
                cache: Some("c".into()),
                continuation: false,
            })
            .collect();
        assert!(overlong_lines(&entries).is_empty());
    }

    #[test]
    fn a_cache_at_two_sample_rates_is_refused_rather_than_mixed() {
        let dir = std::env::temp_dir().join("radio_drama-test-rates");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (s, c) = setup("**MAX:** One.\n\n**VOSS:** Two.\n", ONE_VOICE);
        let plan = Plan::build(&s, &c).unwrap();
        for (i, u) in plan.utterances.iter().enumerate() {
            let rate = if i == 0 { 24_000 } else { 48_000 };
            audio::write_wav(&cache_path(&dir, &u.key), &vec![0.4; 2400], rate).unwrap();
        }
        let err = mix(&s, &c, &plan, &dir, -1.0).unwrap_err().to_string();
        assert!(err.contains("sample rate"), "{err}");
    }
}

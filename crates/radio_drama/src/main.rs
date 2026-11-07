//! `radio_drama` — renders a written radio play into a finished audio
//! drama, in voices cloned from recordings you supply.
//!
//! It reads a script that is already readable as a script, so the input
//! is the document a person would have written anyway:
//!
//! ```text
//! **MAX:** Message from Voss.
//!
//! **VOSS** *(over vox)*: The message does not identify a location.
//!
//! [SFX: Connection closes.]
//! ```
//!
//! and the output is the whole play as one file, with every line spoken
//! in its character's voice, paced like a performance rather than a
//! list, and a manifest saying what happens when.
//!
//! Four commands, in the order they are used:
//!
//! * `cast` — read the script and write a cast file listing every part in
//!   it, for you to assign voices to.
//! * `voices` — what the clone server already has.
//! * `check` — parse, reconcile against the cast, and report. No network.
//! * `render` — synthesize what is missing and mix the play.
//!
//! Music and sound effects are not produced. Every `[SFX]`, `[MUSIC]`
//! and `[TRANSITION]` cue in the script is kept, given a configurable
//! hole of silence to sit in, and written to the manifest with the
//! timestamps of that hole, so a later pass has an exact place to put
//! one.

mod audio;
mod cast;
mod dsp;
mod qwen3;
mod render;
mod script;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use clap::{Parser, Subcommand, ValueEnum};

use cast::{CastFile, ResolvedCast};
use render::{Format, Manifest, Plan};
use script::{ParseOptions, Script};

/// The clone deploy's own variable, used when `--url` and
/// `RADIO_DRAMA_CLONE_URL` are both unset so a checkout already
/// configured for rpg_vox needs nothing added.
const FALLBACK_URL_ENV: &str = "RPG_VOX_QWEN3_CLONE_URL";

#[derive(Parser, Debug)]
#[command(
    name = "radio_drama",
    about = "Render a written radio play into a finished audio drama with cloned voices",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Write a cast file for a script: every speaking part in it, in the
    /// order each is first heard, with nowhere to guess who should sound
    /// like what. Run this first, fill it in, then `render`.
    Cast {
        #[command(flatten)]
        script: ScriptArgs,
        /// Overwrite an existing cast file. Without this an existing file
        /// is read first and everything already assigned is kept, so
        /// re-running after the script gains a character is safe.
        #[arg(long)]
        force: bool,
    },
    /// Parse the script, reconcile it against the cast, and report what a
    /// render would do. Touches no network and writes nothing.
    Check {
        #[command(flatten)]
        script: ScriptArgs,
    },
    /// List the voices the clone server has registered.
    Voices {
        #[command(flatten)]
        server: ServerArgs,
    },
    /// Synthesize what is missing and mix the play.
    Render {
        #[command(flatten)]
        script: ScriptArgs,
        #[command(flatten)]
        server: ServerArgs,
        /// Where the mixdown, the manifest and the cache go. Defaults to
        /// a directory named after the script, beside it.
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = OutFormat::Flac)]
        format: OutFormat,
        /// Synthesis requests in flight at once. The deploy serves 64
        /// sequences per stage, so this is about being a good neighbour
        /// rather than a limit of the server's.
        #[arg(short, long, default_value_t = 8)]
        jobs: usize,
        /// Re-synthesize every line even if it is cached. Needed only
        /// when the server itself has changed under a voice name that
        /// stayed the same.
        #[arg(long)]
        refresh: bool,
        /// Mix from the cache alone and fail if anything is missing.
        /// Re-paces or re-balances a play that is already synthesized
        /// without touching the network.
        #[arg(long)]
        offline: bool,
        /// Level the finished mix is pulled down to if it exceeds it, in
        /// dBFS. A mix under it is left alone.
        #[arg(long, default_value_t = -1.0)]
        peak_db: f32,
    },
}

#[derive(Parser, Debug)]
struct ScriptArgs {
    /// The radio play, as markdown.
    script: PathBuf,
    /// The cast file. Defaults to `<script>.cast.toml` beside the script.
    #[arg(short, long)]
    cast: Option<PathBuf>,
    /// Stop reading the play at the first heading containing this,
    /// instead of at the first top-level heading after the play starts
    /// (which is what ends it at a `# PRODUCTION NOTES` section).
    #[arg(long)]
    end_at: Option<String>,
}

#[derive(Parser, Debug)]
struct ServerArgs {
    /// The Qwen3-TTS clone deploy, e.g. https://qwen3-tts-clone.example.
    #[arg(long, env = "RADIO_DRAMA_CLONE_URL")]
    url: Option<String>,
    /// Per-request timeout, in seconds. The first request after the
    /// container starts waits on CUDA graph capture.
    #[arg(long, default_value_t = 180)]
    timeout: u64,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum OutFormat {
    Flac,
    Wav,
}

impl From<OutFormat> for Format {
    fn from(f: OutFormat) -> Self {
        match f {
            OutFormat::Flac => Format::Flac,
            OutFormat::Wav => Format::Wav,
        }
    }
}

impl ScriptArgs {
    fn cast_path(&self) -> PathBuf {
        self.cast.clone().unwrap_or_else(|| {
            let mut p = self.script.clone();
            p.set_extension("cast.toml");
            p
        })
    }

    fn parse_options(&self) -> ParseOptions {
        ParseOptions {
            end_at: self.end_at.clone(),
        }
    }

    fn read(&self) -> Result<Script> {
        Script::parse_file(&self.script, &self.parse_options())
    }

    /// The script and its cast, reconciled. Uncast parts are reported
    /// but not fatal here: `check` is expected to list them, and it is
    /// [`Plan::build`] that refuses to render with any.
    fn read_with_cast(&self) -> Result<(Script, ResolvedCast)> {
        let script = self.read()?;
        let path = self.cast_path();
        if !path.exists() {
            bail!(
                "no cast file at {}. Run `radio_drama cast {}` to write one.",
                path.display(),
                self.script.display()
            );
        }
        let file = CastFile::load(&path)?;
        let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let resolved = file.resolve(&script, &base)?;
        Ok((script, resolved))
    }
}

impl ServerArgs {
    fn client(&self) -> Result<qwen3::Client> {
        let url = match self.url.clone() {
            Some(url) => url,
            None => std::env::var(FALLBACK_URL_ENV).map_err(|_| {
                anyhow::anyhow!(
                    "no clone server. Pass --url, or set RADIO_DRAMA_CLONE_URL \
                     or {FALLBACK_URL_ENV}."
                )
            })?,
        };
        qwen3::Client::new(&url, Duration::from_secs(self.timeout))
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "radio_drama=warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Cast { script, force } => cmd_cast(&script, force),
        Command::Check { script } => cmd_check(&script),
        Command::Voices { server } => block_on(cmd_voices(server)),
        Command::Render {
            script,
            server,
            out,
            format,
            jobs,
            refresh,
            offline,
            peak_db,
        } => block_on(cmd_render(RenderArgs {
            script,
            server,
            out,
            format: format.into(),
            jobs,
            refresh,
            offline,
            peak_db,
        })),
    }
}

/// Everything here is a batch job that waits on HTTP, so the runtime is
/// built where it is needed rather than wrapping `main` in `#[tokio::main]`
/// and starting one for `cast` and `check`, which never make a request.
fn block_on<F: std::future::Future<Output = Result<()>>>(fut: F) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?
        .block_on(fut)
}

fn cmd_cast(args: &ScriptArgs, force: bool) -> Result<()> {
    let script = args.read()?;
    let path = args.cast_path();
    let existing = match (path.exists(), force) {
        (true, false) => Some(CastFile::load(&path)?),
        (true, true) => None,
        (false, _) => None,
    };
    let text = cast::scaffold(&script, existing.as_ref());
    audio::write_atomic(&path, text.as_bytes())?;

    let speakers = script.speakers();
    let assigned = existing
        .as_ref()
        .map(|c| {
            speakers
                .iter()
                .filter(|s| {
                    c.cast.iter().any(|(written, entry)| {
                        script::normalize_speaker(written) == s.key
                            && (entry.sample.is_some()
                                || entry.server_voice.is_some()
                                || entry.same_as.is_some())
                    })
                })
                .count()
        })
        .unwrap_or(0);

    println!("wrote {}", path.display());
    println!(
        "  {} part{}, {} already cast",
        speakers.len(),
        plural(speakers.len()),
        assigned
    );
    if assigned < speakers.len() {
        println!(
            "  fill in the rest, then: radio_drama render {}",
            args.script.display()
        );
    }
    Ok(())
}

fn cmd_check(args: &ScriptArgs) -> Result<()> {
    let (script, resolved) = args.read_with_cast()?;
    report_script(&script, &resolved, args);

    if !resolved.uncast.is_empty() {
        println!();
        println!(
            "{} part{} with no voice:",
            resolved.uncast.len(),
            plural(resolved.uncast.len())
        );
        for speaker in &resolved.uncast {
            println!("  {:<22} {} line(s)", speaker.display, speaker.lines);
        }
        println!();
        bail!(
            "cast {} more part{} before rendering (edit {})",
            resolved.uncast.len(),
            plural(resolved.uncast.len()),
            args.cast_path().display()
        );
    }

    let plan = Plan::build(&script, &resolved)?;
    report_plan(&plan);
    println!();
    println!("ready to render.");
    Ok(())
}

async fn cmd_voices(server: ServerArgs) -> Result<()> {
    let client = server.client()?;
    let voices = client.voices().await?;
    println!("{} voice(s) on {}", voices.len(), client.base());
    for v in voices {
        println!("  {v}");
    }
    Ok(())
}

struct RenderArgs {
    script: ScriptArgs,
    server: ServerArgs,
    out: Option<PathBuf>,
    format: Format,
    jobs: usize,
    refresh: bool,
    offline: bool,
    peak_db: f32,
}

async fn cmd_render(args: RenderArgs) -> Result<()> {
    let (script, resolved) = args.script.read_with_cast()?;
    report_script(&script, &resolved, &args.script);

    if !resolved.uncast.is_empty() {
        println!();
        for speaker in &resolved.uncast {
            println!("  uncast: {} ({} lines)", speaker.display, speaker.lines);
        }
        bail!(
            "{} part(s) have no voice; run `radio_drama check` for the list",
            resolved.uncast.len()
        );
    }

    let plan = Plan::build(&script, &resolved)?;
    report_plan(&plan);

    let stem = args
        .script
        .script
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "radio_drama".to_string());
    let out_dir = args.out.clone().unwrap_or_else(|| {
        args.script
            .script
            .parent()
            .unwrap_or(Path::new("."))
            .join(&stem)
    });
    let cache_dir = out_dir.join("cache");

    println!();
    if args.offline {
        let missing = plan
            .utterances
            .iter()
            .filter(|u| !render::cache_path(&cache_dir, &u.key).exists())
            .count();
        if missing > 0 {
            bail!(
                "--offline, but {missing} of {} utterances are not in {}",
                plan.utterances.len(),
                cache_dir.display()
            );
        }
        println!(
            "offline: mixing {} cached utterances",
            plan.utterances.len()
        );
    } else {
        let client = args.server.client()?;
        let uploaded = render::ensure_voices(&client, &resolved).await?;
        println!(
            "voices: {} on {}{}",
            resolved.voices.len(),
            client.base(),
            if uploaded > 0 {
                format!(", {uploaded} newly registered")
            } else {
                String::new()
            }
        );
        let stats = render::synthesize(&client, &plan, &cache_dir, args.jobs, args.refresh).await?;
        println!(
            "cache: {} reused, {} synthesized",
            stats.cached, stats.synthesized
        );
    }

    let mix = render::mix(&script, &resolved, &plan, &cache_dir, args.peak_db)?;
    let audio_name = format!("{stem}.{}", args.format.extension());
    let audio_path = out_dir.join(&audio_name);
    match args.format {
        Format::Flac => audio::write_flac(&audio_path, &mix.samples, mix.sample_rate)?,
        Format::Wav => audio::write_wav(&audio_path, &mix.samples, mix.sample_rate)?,
    }
    let manifest_path = out_dir.join(format!("{stem}.manifest.json"));
    Manifest::build(&args.script.script, &script, &resolved, &mix, &audio_name)
        .write(&manifest_path)?;

    println!();
    println!("duration: {}", hms(mix.duration_s()));
    match mix.peak_db {
        Some(peak) => println!(
            "peak:     {peak:.1} dBFS{}",
            if mix.limited_db < 0.0 {
                format!(" (pulled down {:.1} dB)", -mix.limited_db)
            } else {
                String::new()
            }
        ),
        None => println!("peak:     silence"),
    }
    println!(
        "wrote     {} ({})",
        audio_path.display(),
        size_of(&audio_path)
    );
    println!("          {}", manifest_path.display());
    Ok(())
}

fn report_script(script: &Script, resolved: &ResolvedCast, args: &ScriptArgs) {
    let title = resolved
        .production
        .title
        .as_deref()
        .or(script.title.as_deref())
        .unwrap_or("(untitled)");
    println!("{title}");
    if let Some(sub) = &script.subtitle {
        println!("{sub}");
    }
    println!("  {}", args.script.display());
    println!("  {}", args.cast_path().display());

    let scenes = script
        .elements
        .iter()
        .filter(|e| matches!(e, script::Element::Scene(_)))
        .count();
    let lines: Vec<_> = script.lines().collect();
    let words: usize = lines
        .iter()
        .map(|l| l.text.split_whitespace().count())
        .sum();
    println!();
    println!(
        "  {scenes} scene{}, {} line{}, {words} words",
        plural(scenes),
        lines.len(),
        plural(lines.len())
    );

    // Cues, by kind, newest-first by how many there are: this is the
    // shopping list for whoever scores the production later.
    let mut cues: Vec<(String, usize)> = Vec::new();
    let mut pauses = 0;
    for element in &script.elements {
        match element {
            script::Element::Cue { kind, .. } => match cues.iter_mut().find(|(k, _)| k == kind) {
                Some((_, n)) => *n += 1,
                None => cues.push((kind.clone(), 1)),
            },
            script::Element::Pause { .. } => pauses += 1,
            _ => {}
        }
    }
    cues.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    if !cues.is_empty() || pauses > 0 {
        let mut parts: Vec<String> = cues.iter().map(|(k, n)| format!("{n} {k}")).collect();
        if pauses > 0 {
            parts.push(format!("{pauses} PAUSE"));
        }
        println!("  cues left for a later pass: {}", parts.join(", "));
    }

    if !resolved.voices.is_empty() {
        println!();
        println!("Cast");
        for voice in &resolved.voices {
            let source = match &voice.source {
                cast::VoiceSource::Registered => "on the server".to_string(),
                cast::VoiceSource::Sample { path, .. } => path.display().to_string(),
            };
            println!(
                "  {:<34} {:<18} {}",
                voice.characters.join(", "),
                voice.id,
                source
            );
        }
    }
}

fn report_plan(plan: &Plan) {
    println!();
    println!(
        "{} utterance{} to speak, {} distinct",
        plan.speech_steps(),
        plural(plan.speech_steps()),
        plan.utterances.len()
    );
    println!("  silence between them: {}", hms(plan.silence_s() as f64));

    if !plan.unmatched_directions.is_empty() {
        println!();
        println!(
            "{} direction{} the audio cannot honour (the clone model takes no style input):",
            plan.unmatched_directions.len(),
            plural(plan.unmatched_directions.len())
        );
        for d in &plan.unmatched_directions {
            println!("  ({d})");
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn hms(seconds: f64) -> String {
    let total = seconds.round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else {
        format!("{m}m {s:02}s")
    }
}

fn size_of(path: &Path) -> String {
    match std::fs::metadata(path).map(|m| m.len()) {
        Ok(bytes) if bytes >= 1 << 20 => format!("{:.1} MB", bytes as f64 / (1u64 << 20) as f64),
        Ok(bytes) if bytes >= 1 << 10 => format!("{:.1} kB", bytes as f64 / (1u64 << 10) as f64),
        Ok(bytes) => format!("{bytes} B"),
        Err(_) => "?".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cast_file_defaults_to_sitting_beside_the_script() {
        let args = ScriptArgs {
            script: PathBuf::from("/plays/episode_three.md"),
            cast: None,
            end_at: None,
        };
        assert_eq!(
            args.cast_path(),
            PathBuf::from("/plays/episode_three.cast.toml")
        );
    }

    #[test]
    fn an_explicit_cast_file_is_used_as_given() {
        let args = ScriptArgs {
            script: PathBuf::from("/plays/episode_three.md"),
            cast: Some(PathBuf::from("cast.toml")),
            end_at: None,
        };
        assert_eq!(args.cast_path(), PathBuf::from("cast.toml"));
    }

    #[test]
    fn durations_read_as_durations() {
        assert_eq!(hms(0.0), "0m 00s");
        assert_eq!(hms(61.4), "1m 01s");
        assert_eq!(hms(2321.0), "38m 41s");
        assert_eq!(hms(3725.0), "1h 02m 05s");
    }

    #[test]
    fn the_cli_parses_the_four_commands() {
        for argv in [
            vec!["radio_drama", "cast", "a.md"],
            vec!["radio_drama", "check", "a.md", "--cast", "c.toml"],
            vec!["radio_drama", "voices", "--url", "https://x.test"],
            vec![
                "radio_drama",
                "render",
                "a.md",
                "--offline",
                "--format",
                "wav",
            ],
        ] {
            Cli::try_parse_from(argv.clone()).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
        }
    }
}

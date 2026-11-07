//! Reading and writing the files a render touches: cached utterances,
//! reference recordings, and the mixdown.
//!
//! Everything in between is mono `f32` at the synthesizer's own rate,
//! which is the one rate in the pipeline. The clone deploy returns 24 kHz
//! mono, a play is speech, and resampling it to something rounder would
//! cost quality and size to gain nothing — so the rate the first
//! utterance comes back at is the rate the mixdown is written at, and
//! the manifest records it.

use std::path::Path;

use anyhow::{bail, Context as _, Result};

#[derive(Debug)]
pub struct Pcm {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Read a wav keeping its channels: interleaved samples, the channel
/// count, and the rate. Handles the integer and float formats hound
/// knows, so a cache written by a future server that returns stereo or
/// 32-bit float still reads back.
pub fn read_wav_interleaved(path: &Path) -> Result<(Vec<f32>, usize, u32)> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("opening wav {}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .with_context(|| format!("reading {}", path.display()))?,
        hound::SampleFormat::Int => {
            let full = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|s| s as f32 / full))
                .collect::<Result<_, _>>()
                .with_context(|| format!("reading {}", path.display()))?
        }
    };
    Ok((interleaved, channels, spec.sample_rate))
}

/// Average the channels of an interleaved buffer down to one.
fn to_mono(interleaved: Vec<f32>, channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved;
    }
    interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Read a wav into mono `f32`.
pub fn read_wav(path: &Path) -> Result<Pcm> {
    let (interleaved, channels, sample_rate) = read_wav_interleaved(path)?;
    Ok(Pcm {
        samples: to_mono(interleaved, channels),
        sample_rate,
    })
}

/// Two channels of `f32`, as long as each other.
///
/// The speech-only mix is mono and stays that way; this exists for the
/// scored master, where the dialogue goes up the middle and the cues
/// keep the width their composer gave them.
#[derive(Debug, Clone)]
pub struct Stereo {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Stereo {
    pub fn silence(frames: usize) -> Self {
        Self {
            left: vec![0.0; frames],
            right: vec![0.0; frames],
        }
    }

    pub fn frames(&self) -> usize {
        self.left.len().min(self.right.len())
    }

    pub fn gain(&mut self, g: f32) {
        for s in self.left.iter_mut().chain(self.right.iter_mut()) {
            *s *= g;
        }
    }

    pub fn fade_in(&mut self, frames: usize) {
        let frames = frames.min(self.frames());
        for i in 0..frames {
            let g = i as f32 / frames as f32;
            self.left[i] *= g;
            self.right[i] *= g;
        }
    }

    pub fn fade_out(&mut self, frames: usize) {
        let n = self.frames();
        let frames = frames.min(n);
        for i in 0..frames {
            let g = i as f32 / frames as f32;
            self.left[n - 1 - i] *= g;
            self.right[n - 1 - i] *= g;
        }
    }

    pub fn peak_db(&self) -> Option<f32> {
        let peak = self
            .left
            .iter()
            .chain(self.right.iter())
            .fold(0.0f32, |m, s| m.max(s.abs()));
        (peak > 0.0).then(|| 20.0 * peak.log10())
    }

    /// Pull the whole thing down if it exceeds `target_db`; leave it
    /// alone if it does not. Returns the gain applied, in dB.
    pub fn limit_peak(&mut self, target_db: f32) -> f32 {
        match self.peak_db() {
            Some(peak) if peak > target_db => {
                self.gain(crate::dsp::db_to_gain(target_db - peak));
                target_db - peak
            }
            _ => 0.0,
        }
    }

    /// Interleaved `[L0, R0, L1, R1, …]`, which is the layout both
    /// encoders want.
    fn interleaved(&self) -> Vec<f32> {
        let n = self.frames();
        let mut out = Vec::with_capacity(n * 2);
        for i in 0..n {
            out.push(self.left[i]);
            out.push(self.right[i]);
        }
        out
    }
}

pub struct DecodedStereo {
    pub audio: Stereo,
    pub rate: u32,
}

/// Decode a file keeping both channels. A mono file comes back with the
/// same signal in each, which is what a mono cue should sound like
/// rather than a cue only on the left.
pub fn decode_stereo(path: &Path) -> Result<DecodedStereo> {
    let (interleaved, channels, rate) = decode_interleaved(path)?;
    let frames = interleaved.len() / channels.max(1);
    let mut audio = Stereo::silence(frames);
    for i in 0..frames {
        let frame = &interleaved[i * channels..(i + 1) * channels];
        match channels {
            1 => {
                audio.left[i] = frame[0];
                audio.right[i] = frame[0];
            }
            _ => {
                audio.left[i] = frame[0];
                audio.right[i] = frame[1];
            }
        }
    }
    Ok(DecodedStereo { audio, rate })
}

/// Sinc resampling, the same settings rpg_vox uses. Identity when the
/// rates already agree, which is the common case for a cue that was
/// bounced at the mastering rate.
pub fn resample_mono(samples: &[f32], from: u32, to: u32) -> Result<Vec<f32>> {
    if from == to || samples.is_empty() {
        return Ok(samples.to_vec());
    }
    use rubato::{
        Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
    };
    let params = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    let mut rs = SincFixedIn::<f32>::new(to as f64 / from as f64, 2.0, params, 1024, 1)
        .context("building resampler")?;
    let chunk = rs.input_frames_next();
    let mut out = Vec::with_capacity(samples.len() * to as usize / from as usize + chunk);
    for block in samples.chunks(chunk) {
        let mut padded;
        let slice: &[f32] = if block.len() == chunk {
            block
        } else {
            padded = vec![0.0f32; chunk];
            padded[..block.len()].copy_from_slice(block);
            &padded
        };
        let done = rs.process(&[slice], None).context("resampling")?;
        out.extend_from_slice(&done[0]);
    }
    // The tail is whatever the last padded block produced past the real
    // input; trim to the length the ratio implies so a cue does not
    // grow by a few milliseconds per pass.
    let want = (samples.len() as f64 * to as f64 / from as f64).round() as usize;
    out.truncate(want);
    Ok(out)
}

pub fn resample_stereo(audio: &Stereo, from: u32, to: u32) -> Result<Stereo> {
    if from == to {
        return Ok(audio.clone());
    }
    Ok(Stereo {
        left: resample_mono(&audio.left, from, to)?,
        right: resample_mono(&audio.right, from, to)?,
    })
}

/// Decode a reference recording the operator supplied, to mono `f32`.
///
/// symphonia covers wav, flac, mp3 and ogg/vorbis. What it does not
/// cover is Ogg Opus — it has no Opus decoder, and the one that works
/// is a C library — so anything it refuses is handed to `ffmpeg`, which
/// is already needed for the default output format. That keeps a clip
/// pulled straight out of a scribe recording usable without this crate
/// having to link anything.
pub fn decode_reference(path: &Path) -> Result<Pcm> {
    let (interleaved, channels, sample_rate) = decode_interleaved(path)?;
    Ok(Pcm {
        samples: to_mono(interleaved, channels),
        sample_rate,
    })
}

fn decode_with_symphonia(path: &Path) -> Result<(Vec<f32>, usize, u32)> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error as SymError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("unrecognized audio format")?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| anyhow::anyhow!("no audio track"))?;
    let track_id = track.id;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| anyhow::anyhow!("unknown sample rate"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("no decoder for this codec")?;

    let mut samples = Vec::new();
    let mut channels = 1usize;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // A damaged packet mid-file is worth skipping rather than
            // abandoning a reference that is otherwise fine.
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        channels = spec.channels.count().max(1);
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        samples.extend_from_slice(buf.samples());
    }
    Ok((samples, channels, sample_rate))
}

fn decode_via_ffmpeg(path: &Path, why: &str) -> Result<(Vec<f32>, usize, u32)> {
    let staged = std::env::temp_dir().join(format!(
        "radio_drama-decode-{}-{}.wav",
        std::process::id(),
        // Two decodes in one process must not share a staging path.
        path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    let out = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(path)
        .args(["-c:a", "pcm_s16le"])
        .arg(&staged)
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => anyhow::anyhow!(
                "cannot read {} ({why}), and ffmpeg is not on PATH to convert it. \
                 Supply it as wav, flac, mp3 or ogg/vorbis, or install ffmpeg.",
                path.display()
            ),
            _ => anyhow::anyhow!("running ffmpeg on {}: {e}", path.display()),
        })?;
    let result = (|| {
        if !out.status.success() {
            bail!(
                "cannot read {} ({why}), and ffmpeg could not convert it either: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let (interleaved, channels, rate) = read_wav_interleaved(&staged)?;
        if interleaved.is_empty() {
            bail!("{} decoded to no samples", path.display());
        }
        Ok((interleaved, channels, rate))
    })();
    let _ = std::fs::remove_file(&staged);
    result
}

/// Decode anything, keeping its channels: symphonia first, ffmpeg for
/// what it refuses.
fn decode_interleaved(path: &Path) -> Result<(Vec<f32>, usize, u32)> {
    if !path.exists() {
        bail!("{} does not exist", path.display());
    }
    match decode_with_symphonia(path) {
        Ok((samples, ch, rate)) if !samples.is_empty() => Ok((samples, ch, rate)),
        // An empty decode counts as a failure: a reference with no audio
        // in it registers a voice that sounds like nothing.
        Ok(_) => decode_via_ffmpeg(path, "it decoded to no samples"),
        Err(e) => decode_via_ffmpeg(path, &format!("{e:#}")),
    }
}

/// Decode a reference recording and re-encode it as the 16-bit mono wav
/// the clone server takes.
pub fn reference_as_wav(path: &Path) -> Result<Vec<u8>> {
    let pcm = decode_reference(path)?;
    encode_wav_i16(&pcm.samples, pcm.sample_rate)
}

/// 16-bit mono wav, in memory.
pub fn encode_wav_i16(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    encode_wav_i16_ch(samples, 1, sample_rate)
}

/// 16-bit wav of `channels` interleaved channels, in memory.
pub fn encode_wav_i16_ch(samples: &[f32], channels: u16, sample_rate: u32) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut out, spec).context("starting wav encoder")?;
        for &s in samples {
            writer
                .write_sample(to_i16(s))
                .context("writing wav sample")?;
        }
        writer.finalize().context("finalizing wav")?;
    }
    Ok(out.into_inner())
}

pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) -> Result<()> {
    let bytes = encode_wav_i16(samples, sample_rate)?;
    write_atomic(path, &bytes)
}

pub fn write_wav_stereo(path: &Path, audio: &Stereo, sample_rate: u32) -> Result<()> {
    let bytes = encode_wav_i16_ch(&audio.interleaved(), 2, sample_rate)?;
    write_atomic(path, &bytes)
}

/// FLAC, through the same staged-wav-and-ffmpeg path as the mono
/// writer. See [`write_flac`] for why it is not encoded in process.
pub fn write_flac_stereo(path: &Path, audio: &Stereo, sample_rate: u32) -> Result<()> {
    let staged = path.with_extension("staging.wav");
    write_wav_stereo(&staged, audio, sample_rate)?;
    let result = run_ffmpeg_flac(&staged, path);
    let _ = std::fs::remove_file(&staged);
    result
}

/// Write the mixdown as FLAC, by writing the wav and handing it to
/// `ffmpeg`.
///
/// Encoding it in-process would be preferable and was how this started,
/// but flacenc 0.5.1 writes frame numbers that `flac -t` reports as
/// "sample or frame number does not increase correctly … file might not
/// be seekable", and that symphonia — this repo's own decoder, via
/// [`decode_reference`] — refuses outright. A forty-minute drama
/// that a player cannot scrub through is not an acceptable default, and
/// an output file this crate cannot read back would block laying music
/// under it later. ffmpeg is already required here (`shell.nix`, and
/// `discord_vox --record` transcodes its session audio with it), so
/// this uses it rather than shipping a known-bad encoder.
pub fn write_flac(path: &Path, samples: &[f32], sample_rate: u32) -> Result<()> {
    // Beside the destination rather than in a temp dir, so the rename
    // ffmpeg's output goes through stays on one filesystem.
    let staged = path.with_extension("staging.wav");
    write_wav(&staged, samples, sample_rate)?;
    let result = run_ffmpeg_flac(&staged, path);
    let _ = std::fs::remove_file(&staged);
    result
}

fn run_ffmpeg_flac(wav: &Path, flac: &Path) -> Result<()> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(wav)
        .args(["-c:a", "flac", "-compression_level", "8"])
        .arg(flac)
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => anyhow::anyhow!(
                "ffmpeg is needed to write FLAC and is not on PATH. \
                 Install it, or render with --format wav."
            ),
            _ => anyhow::anyhow!("running ffmpeg: {e}"),
        })?;
    if !out.status.success() {
        bail!(
            "ffmpeg failed to encode {}: {}",
            flac.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

/// Write through a temporary file in the same directory and rename, so a
/// render interrupted part-way through never leaves a half-written file
/// that a later run would read back as if it were whole. The cache in
/// particular depends on this: a truncated entry there would be a wrong
/// answer rather than a missing one.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = path.with_extension(format!(
        "{}.part",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} into place", tmp.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("radio_drama-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_wav_round_trips_through_16_bit() {
        let dir = tmpdir("wav-roundtrip");
        let path = dir.join("a.wav");
        let samples: Vec<f32> = (0..480)
            .map(|i| (i as f32 / 480.0 * std::f32::consts::TAU).sin() * 0.5)
            .collect();
        write_wav(&path, &samples, 24_000).unwrap();
        let back = read_wav(&path).unwrap();
        assert_eq!(back.sample_rate, 24_000);
        assert_eq!(back.samples.len(), samples.len());
        for (a, b) in samples.iter().zip(&back.samples) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    /// Needs `ffmpeg` on PATH, which the dev shell provides; skipped
    /// rather than failed where it is absent, so the rest of the suite
    /// still runs.
    #[test]
    fn flac_is_written_and_decodes_to_the_same_audio() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("skipping: no ffmpeg on PATH");
            return;
        }
        let dir = tmpdir("flac-roundtrip");
        let path = dir.join("a.flac");
        // A second of audio: symphonia's prober wants more bytes than a
        // few frames of FLAC occupy before it will identify the stream
        // at all, so a shorter case here would be testing the prober's
        // minimum rather than the encoder.
        let samples: Vec<f32> = (0..24_000)
            .map(|i| (i as f32 / 100.0 * std::f32::consts::TAU).sin() * 0.4)
            .collect();
        write_flac(&path, &samples, 24_000).unwrap();

        // Read back through the same decoder a reference sample uses:
        // an output file this crate cannot open is the failure mode
        // that moved this off an in-process encoder in the first place.
        let back = decode_reference(&path).unwrap();
        assert_eq!(back.sample_rate, 24_000);
        assert_eq!(back.samples.len(), samples.len());
        for (a, b) in samples.iter().zip(&back.samples) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }

        // The wav handed to ffmpeg does not outlive the call.
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "a.flac")
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    fn have_ffmpeg() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }

    /// Ogg Opus is the format symphonia has no decoder for, so this is
    /// the case the ffmpeg fallback exists to cover — and a clip taken
    /// out of a scribe recording is exactly what it would be.
    #[test]
    fn an_ogg_opus_reference_goes_through_ffmpeg() {
        if !have_ffmpeg() {
            eprintln!("skipping: no ffmpeg on PATH");
            return;
        }
        let dir = tmpdir("opus-reference");
        let wav = dir.join("source.wav");
        let samples: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 / 48.0 * std::f32::consts::TAU).sin() * 0.4)
            .collect();
        write_wav(&wav, &samples, 48_000).unwrap();
        let opus = dir.join("reference.opus");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&wav)
            .args(["-c:a", "libopus"])
            .arg(&opus)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("skipping: this ffmpeg cannot encode Opus");
            return;
        }

        // symphonia on its own does not get there.
        assert!(decode_with_symphonia(&opus).is_err());

        let pcm = decode_reference(&opus).unwrap();
        assert!(!pcm.samples.is_empty());
        // Opus always decodes at 48 kHz and is lossy, so the check is
        // that the tone survived, not that the samples match.
        assert_eq!(pcm.sample_rate, 48_000);
        assert!(crate::dsp::peak_db(&pcm.samples).unwrap() > -12.0);

        // And the wav it staged for ffmpeg does not outlive the call.
        let left: Vec<String> = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("radio_drama-reference-"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn a_missing_reference_names_the_path() {
        let err = decode_reference(Path::new("/nonexistent/voices/nobody.wav"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("nobody.wav"), "{err}");
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    fn a_reference_that_is_not_audio_says_so_rather_than_registering_nothing() {
        let dir = tmpdir("bad-reference");
        let path = dir.join("notes.wav");
        std::fs::write(&path, b"this is not audio at all, it is a text file").unwrap();
        let err = decode_reference(&path).unwrap_err().to_string();
        assert!(err.contains("notes.wav"), "{err}");
    }

    #[test]
    fn stereo_and_float_wavs_read_back_as_mono() {
        let dir = tmpdir("stereo");
        let path = dir.join("s.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..10 {
            w.write_sample(1.0f32).unwrap();
            w.write_sample(0.0f32).unwrap();
        }
        w.finalize().unwrap();
        let back = read_wav(&path).unwrap();
        assert_eq!(back.sample_rate, 48_000);
        assert_eq!(back.samples.len(), 10);
        assert!((back.samples[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn an_atomic_write_leaves_no_part_file_behind() {
        let dir = tmpdir("atomic");
        let path = dir.join("x.wav");
        write_atomic(&path, b"hello").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("part"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn an_atomic_write_creates_the_directory() {
        let dir = tmpdir("atomic-mkdir");
        let path = dir.join("nested/deeper/x.wav");
        write_atomic(&path, b"hi").unwrap();
        assert!(path.exists());
    }
}

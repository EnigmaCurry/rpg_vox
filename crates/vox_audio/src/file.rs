//! Decode an audio file (wav, flac, mp3, ogg/vorbis, ogg/opus) to mono
//! f32. Symphonia has no Opus decoder, so Ogg Opus goes through libopus
//! ([`crate::opus_file`]).

use std::path::Path;

use anyhow::{anyhow, Context as _, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub struct Decoded {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// True when `path` starts with an Ogg page carrying an Opus header.
fn is_ogg_opus(path: &Path) -> bool {
    let mut head = [0u8; 64];
    let n = std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
        .unwrap_or(0);
    head[..n].starts_with(b"OggS") && head[..n].windows(8).any(|w| w == b"OpusHead")
}

fn decode_opus(path: &Path) -> Result<Decoded> {
    let file = crate::opus_file::OpusFile::open(path)?;
    let mut cursor = file.cursor(0)?;
    let mut samples = Vec::with_capacity(file.len as usize);
    while cursor.read(&mut samples)? {}
    Ok(Decoded {
        samples,
        sample_rate: crate::opus_file::RATE,
    })
}

pub fn decode(path: &Path) -> Result<Decoded> {
    if is_ogg_opus(path) {
        return decode_opus(path);
    }
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("unrecognized audio format")?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| anyhow!("no audio track"))?;
    let track_id = track.id;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| anyhow!("unknown sample rate"))?;
    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let mut samples = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks_exact(channels) {
            samples.push(frame.iter().sum::<f32>() / channels as f32);
        }
    }
    Ok(Decoded {
        samples,
        sample_rate,
    })
}

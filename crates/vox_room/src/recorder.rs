//! A recording of the room, built as it plays.
//!
//! One thread, one channel of PCM frames in from the mixer, one
//! Ogg/Opus file out at the end. The frames are the room's own mix --
//! see [`crate::mixer::Mixer::record_to`] for what is and is not in it
//! -- and they arrive already at 48 kHz mono in 20 ms frames, which is
//! exactly one Opus packet each, so nothing here resamples, buffers or
//! re-encodes anything.
//!
//! **Opus and not a WAV**, which is the one thing a reader of
//! `store.rs` might expect instead. A voice sample is a few seconds and
//! goes to a clone server that wants the raw samples; a recording is as
//! long as a meeting, and it is kept in the database beside the
//! conversation it was made in. An hour of 48 kHz mono WAV is 345 MB
//! there. The same hour of Opus is about ten, and every browser plays
//! it.
//!
//! The encoder is a second one, beside the per-listener encoders in the
//! mixer, and deliberately not one of them. A listener's mix is the
//! room minus their own voice, with the AI in their language and their
//! own choice of interpreter in it; recording through one of those
//! would be recording a pair of headphones, and getting a WAV out of it
//! would mean decoding Opus back to PCM to encode it again.

use std::io::Cursor;

use anyhow::{Context as _, Result};
use crossbeam_channel::Receiver;
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use tracing::{info, warn};

use crate::mixer::{FRAME, RATE};

/// What the room is recorded at. Speech, so a bitrate well under music:
/// Opus at 32 kbit/s mono is clean for a voice and makes an hour of
/// room about 14 MB.
const BITRATE: i32 = 32_000;

/// Samples of silence the decoder is to throw away at the start, which
/// is the encoder's own latency. Written into `OpusHead` so a player
/// lines the audio up with zero, per RFC 7845 §4.2.
const PRE_SKIP: u16 = 312;

/// Take frames off `frames` until the sender goes, then hand the whole
/// recording to `done`: the Ogg/Opus bytes and how long they run.
///
/// The thread ends when the channel closes, which is what stopping a
/// recording does -- the room drops its end and has nothing to wait
/// for. `done` is called exactly once, on this thread, whether the
/// encoding worked or not: a card showing a light on a microphone has
/// to be answered either way.
pub fn spawn(
    id: &str,
    frames: Receiver<Vec<f32>>,
    done: impl FnOnce(Result<(Vec<u8>, f32)>) + Send + 'static,
) -> Result<()> {
    let id = id.to_string();
    std::thread::Builder::new()
        .name("vox-room-recorder".into())
        .spawn(move || {
            let out = write(&frames);
            match &out {
                Ok((bytes, secs)) => info!(
                    recording = %id,
                    secs = *secs as u64,
                    bytes = bytes.len(),
                    "recorded the room"
                ),
                Err(err) => warn!(recording = %id, "recording the room: {err:#}"),
            }
            done(out);
        })
        .context("spawn the recording thread")?;
    Ok(())
}

/// Every frame there is, as one Ogg stream.
fn write(frames: &Receiver<Vec<f32>>) -> Result<(Vec<u8>, f32)> {
    let mut encoder = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Audio)
        .context("opus encoder for the recording")?;
    let _ = encoder.set_bitrate(opus::Bitrate::Bits(BITRATE));
    let mut ogg = PacketWriter::new(Cursor::new(Vec::new()));
    // One stream in the file, so any serial will do as long as it is
    // the same on every page.
    let serial = 1u32;
    ogg.write_packet(head(), serial, PacketWriteEndInfo::EndPage, 0)
        .context("write OpusHead")?;
    ogg.write_packet(tags(), serial, PacketWriteEndInfo::EndPage, 0)
        .context("write OpusTags")?;
    let mut packet = [0u8; 4000];
    // How many samples the stream holds at the end of each packet,
    // which is what an Ogg page's granule position is: the position a
    // player is at once it has decoded that far.
    let mut granule = 0u64;
    // The packet before this one, held back so that the last one of
    // all can be written as the end of the stream. A stream with no
    // end-of-stream page on it is a file some players will not seek
    // in and others will not say the length of.
    let mut held: Option<(Vec<u8>, u64)> = None;
    let mut dropped = 0usize;
    for frame in frames {
        // The mixer sends whole frames and nothing else; a short one
        // would make a packet of a different length, which the granule
        // count above would then be wrong about.
        if frame.len() != FRAME {
            dropped += 1;
            continue;
        }
        let n = match encoder.encode_float(&frame, &mut packet) {
            Ok(n) => n,
            Err(err) => {
                // One bad frame is a gap, not the end of a recording
                // somebody is still making.
                warn!("opus encode failed while recording: {err}");
                dropped += 1;
                continue;
            }
        };
        granule += FRAME as u64;
        if let Some((was, at)) = held.replace((packet[..n].to_vec(), granule)) {
            ogg.write_packet(was, serial, PacketWriteEndInfo::NormalPacket, at)
                .context("write a recorded packet")?;
        }
    }
    if let Some((last, at)) = held.take() {
        ogg.write_packet(last, serial, PacketWriteEndInfo::EndStream, at)
            .context("end the recording's stream")?;
    }
    if dropped > 0 {
        warn!(frames = dropped, "frames left out of a recording");
    }
    // The pre-skip is silence the player throws away, so it is not
    // part of what anybody hears.
    let secs = granule.saturating_sub(PRE_SKIP as u64) as f32 / RATE as f32;
    Ok((ogg.into_inner().into_inner(), secs))
}

/// The identification header, RFC 7845 §5.1.
fn head() -> Vec<u8> {
    let mut buf = Vec::with_capacity(19);
    buf.extend_from_slice(b"OpusHead");
    buf.push(1); // version
    buf.push(1); // one channel
    buf.extend_from_slice(&PRE_SKIP.to_le_bytes());
    buf.extend_from_slice(&RATE.to_le_bytes()); // the rate it was taken at
    buf.extend_from_slice(&0i16.to_le_bytes()); // output gain: leave it alone
    buf.push(0); // channel mapping family 0
    buf
}

/// The comment header, RFC 7845 §5.2: who made it and nothing else.
fn tags() -> Vec<u8> {
    let vendor = b"vox_room";
    let mut buf = Vec::with_capacity(8 + 4 + vendor.len() + 4);
    buf.extend_from_slice(b"OpusTags");
    buf.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    buf.extend_from_slice(vendor);
    buf.extend_from_slice(&0u32.to_le_bytes()); // no user comments
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A second of tone, in the frames the mixer sends.
    fn tone(secs: f32) -> Vec<Vec<f32>> {
        let frames = (RATE as f32 * secs / FRAME as f32) as usize;
        (0..frames)
            .map(|i| {
                (0..FRAME)
                    .map(|j| {
                        let t = (i * FRAME + j) as f32 / RATE as f32;
                        0.3 * (t * 440.0 * std::f32::consts::TAU).sin()
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_recording_is_an_ogg_opus_file_as_long_as_what_went_in() {
        let (tx, rx) = crossbeam_channel::unbounded();
        for frame in tone(2.0) {
            tx.send(frame).expect("the recorder is listening");
        }
        drop(tx);
        let (bytes, secs) = write(&rx).expect("a recording");
        // A file, and one a player can tell what it is from byte zero.
        assert_eq!(&bytes[..4], b"OggS", "an Ogg stream");
        assert!(
            bytes.windows(8).any(|w| w == b"OpusHead"),
            "with the Opus identification header in it"
        );
        assert!(bytes.windows(8).any(|w| w == b"OpusTags"));
        // Two seconds of speech-rate Opus is a few kilobytes, and
        // nothing like the 192 KB the same audio is as a WAV.
        assert!(
            (3_000..40_000).contains(&bytes.len()),
            "{} bytes",
            bytes.len()
        );
        assert!((secs - 2.0).abs() < 0.05, "{secs}s");
    }

    #[test]
    fn a_recording_nobody_said_anything_into_is_still_a_file() {
        let (tx, rx) = crossbeam_channel::unbounded::<Vec<f32>>();
        drop(tx);
        let (bytes, secs) = write(&rx).expect("a recording");
        assert_eq!(&bytes[..4], b"OggS", "the headers are still written");
        assert_eq!(secs, 0.0);
    }

    /// The mixer sends whole frames; anything else would put the
    /// granule count out, which is what a player seeks by.
    #[test]
    fn a_frame_of_the_wrong_length_is_left_out() {
        let (tx, rx) = crossbeam_channel::unbounded();
        tx.send(vec![0.0; FRAME / 2]).expect("sent");
        for frame in tone(0.5) {
            tx.send(frame).expect("sent");
        }
        drop(tx);
        let (_, secs) = write(&rx).expect("a recording");
        assert!((secs - 0.5).abs() < 0.05, "{secs}s");
    }
}

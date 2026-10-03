//! Ogg Opus recordings: mono speech at 48 kHz, 20 ms packets. Reading
//! also takes stereo files (any Ogg Opus with the standard channel
//! mapping), decoded straight to mono.
//!
//! [`OpusWriter`] encodes the same samples the transcriber saw, so time
//! zero of the file is time zero of the transcript. [`OpusFile`] keeps
//! a recording's packets in memory (a few MB per hour) and decodes any
//! stretch of it on demand, which is all playback and seeking need.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use anyhow::{bail, Context as _, Result};
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use opus::{Application, Bitrate, Channels, Decoder, Encoder};

use crate::resample::Linear;

/// Opus always runs on a 48 kHz clock (granule positions included).
pub const RATE: u32 = 48_000;
const FRAME: usize = 960;
const BITRATE: i32 = 24_000;
const SERIAL: u32 = 0x766f_7801;

pub struct OpusWriter {
    enc: Encoder,
    writer: PacketWriter<'static, BufWriter<File>>,
    resampler: Linear,
    /// 48 kHz samples not yet encoded (less than one frame).
    pending: Vec<f32>,
    /// Encoded but not yet written, so the last one can end the stream.
    held: Option<(Vec<u8>, u64)>,
    pre_skip: u64,
    /// Real (unpadded) 48 kHz samples taken in.
    samples: u64,
    /// 48 kHz samples encoded, padding and pre-skip included.
    encoded: u64,
}

impl OpusWriter {
    /// Create `path` for mono audio arriving at `input_rate`.
    pub fn create(path: &Path, input_rate: u32) -> Result<Self> {
        let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
        let mut enc =
            Encoder::new(RATE, Channels::Mono, Application::Voip).context("create Opus encoder")?;
        enc.set_bitrate(Bitrate::Bits(BITRATE))
            .context("set Opus bitrate")?;
        let pre_skip = enc.get_lookahead().context("Opus lookahead")?.max(0) as u64;
        let mut writer = PacketWriter::new(BufWriter::new(file));
        writer.write_packet(
            opus_head(pre_skip as u16, input_rate),
            SERIAL,
            PacketWriteEndInfo::EndPage,
            0,
        )?;
        writer.write_packet(opus_tags(), SERIAL, PacketWriteEndInfo::EndPage, 0)?;
        Ok(Self {
            enc,
            writer,
            resampler: Linear::new(input_rate, RATE),
            // The encoder delays its output by its lookahead; decoders
            // drop that much (pre-skip), so sample 0 stays at time 0.
            pending: Vec::with_capacity(FRAME * 2),
            held: None,
            pre_skip,
            samples: 0,
            encoded: 0,
        })
    }

    pub fn write(&mut self, mono: &[f32]) -> Result<()> {
        let before = self.pending.len();
        self.resampler.process(mono, &mut self.pending);
        self.samples += (self.pending.len() - before) as u64;
        self.drain(false)
    }

    /// Pad the last frame, end the stream and flush the file.
    pub fn finish(mut self) -> Result<()> {
        // Encode until every real sample has come out past the pre-skip.
        while self.encoded < self.pre_skip + self.samples || !self.pending.is_empty() {
            self.pending.resize(self.pending.len().max(FRAME), 0.0);
            self.drain(true)?;
        }
        if let Some((pkt, _)) = self.held.take() {
            let end = self.pre_skip + self.samples;
            self.writer
                .write_packet(pkt, SERIAL, PacketWriteEndInfo::EndStream, end)?;
        }
        self.writer.inner_mut().get_mut().sync_data()?;
        Ok(())
    }

    fn drain(&mut self, finishing: bool) -> Result<()> {
        let mut out = vec![0u8; 4000];
        let mut start = 0;
        while self.pending.len() - start >= FRAME {
            let n = self
                .enc
                .encode_float(&self.pending[start..start + FRAME], &mut out)
                .context("Opus encode")?;
            start += FRAME;
            self.encoded += FRAME as u64;
            if let Some((pkt, gp)) = self.held.take() {
                self.writer
                    .write_packet(pkt, SERIAL, PacketWriteEndInfo::NormalPacket, gp)?;
            }
            self.held = Some((out[..n].to_vec(), self.encoded));
        }
        self.pending.drain(..start);
        if finishing && self.encoded >= self.pre_skip + self.samples {
            self.pending.clear();
        }
        Ok(())
    }
}

fn opus_head(pre_skip: u16, input_rate: u32) -> Vec<u8> {
    let mut h = b"OpusHead".to_vec();
    h.push(1); // version
    h.push(1); // channels
    h.extend_from_slice(&pre_skip.to_le_bytes());
    h.extend_from_slice(&input_rate.to_le_bytes());
    h.extend_from_slice(&0i16.to_le_bytes()); // output gain
    h.push(0); // mapping family
    h
}

fn opus_tags() -> Vec<u8> {
    let vendor = concat!("vox_scribe ", env!("CARGO_PKG_VERSION"));
    let mut t = b"OpusTags".to_vec();
    t.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    t.extend_from_slice(vendor.as_bytes());
    t.extend_from_slice(&0u32.to_le_bytes());
    t
}

/// A recording's packets, ready for random-access decoding.
pub struct OpusFile {
    packets: Vec<Vec<u8>>,
    /// Start of each packet on the output timeline (pre-skip removed;
    /// the first packets start before zero).
    starts: Vec<i64>,
    /// Playable length in 48 kHz samples.
    pub len: u64,
}

impl OpusFile {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mut reader = ogg::reading::PacketReader::new(std::io::BufReader::new(file));
        let mut packets = Vec::new();
        let mut pre_skip = None;
        let mut last_gp = 0u64;
        while let Some(pkt) = reader.read_packet().context("read Ogg")? {
            if pre_skip.is_none() {
                if !pkt.data.starts_with(b"OpusHead") || pkt.data.len() < 19 {
                    bail!("{} is not an Ogg Opus file", path.display());
                }
                // A mono decoder downmixes a stereo stream itself; more
                // channels need the multistream API.
                let (channels, mapping) = (pkt.data[9], pkt.data[18]);
                if !(1..=2).contains(&channels) || mapping != 0 {
                    bail!(
                        "{}: only mono and stereo Opus is supported ({channels} channels)",
                        path.display()
                    );
                }
                pre_skip = Some(u16::from_le_bytes([pkt.data[10], pkt.data[11]]) as i64);
                continue;
            }
            if pkt.data.starts_with(b"OpusTags") {
                continue;
            }
            last_gp = last_gp.max(pkt.absgp_page());
            packets.push(pkt.data);
        }
        let pre_skip = pre_skip.context("empty Ogg file")?;
        let mut starts = Vec::with_capacity(packets.len());
        let mut at = -pre_skip;
        for p in &packets {
            starts.push(at);
            at += opus::packet::get_nb_samples(p, RATE).context("Opus packet")? as i64;
        }
        let decoded = (at.max(0)) as u64;
        let len = if last_gp > 0 {
            (last_gp as i64 - pre_skip).clamp(0, decoded as i64) as u64
        } else {
            decoded
        };
        Ok(Self {
            packets,
            starts,
            len,
        })
    }

    /// A decoder positioned at `pos` (48 kHz samples).
    pub fn cursor(&self, pos: u64) -> Result<Cursor<'_>> {
        let pos = pos.min(self.len) as i64;
        let idx = self.starts.partition_point(|&s| s <= pos).saturating_sub(1);
        // Decode a few packets before the target so the decoder state
        // has converged by the time real output starts.
        let warm = idx.saturating_sub(4);
        let mut c = Cursor {
            file: self,
            dec: Decoder::new(RATE, Channels::Mono).context("create Opus decoder")?,
            next: warm,
            skip: 0,
            pos: pos as u64,
        };
        let mut scratch = Vec::new();
        while c.next < idx {
            c.decode_next(&mut scratch)?;
            scratch.clear();
        }
        c.skip = (pos - self.starts.get(idx).copied().unwrap_or(0)).max(0) as usize;
        Ok(c)
    }
}

/// Sequential decoder over an [`OpusFile`].
pub struct Cursor<'a> {
    file: &'a OpusFile,
    dec: Decoder,
    next: usize,
    /// Samples at the head of the next packet that precede the cursor.
    skip: usize,
    /// Output position of the next sample [`Cursor::read`] returns.
    pos: u64,
}

impl Cursor<'_> {
    /// Append the next packet's samples to `out`; `false` at the end.
    pub fn read(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        loop {
            if self.next >= self.file.packets.len() || self.pos >= self.file.len {
                return Ok(false);
            }
            let before = out.len();
            let start = self.file.starts[self.next];
            self.decode_next(out)?;
            // Trim pre-skip (negative timeline), the seek offset, and
            // padding past the end.
            let mut head = self.skip;
            if start < 0 {
                head = head.max((-start) as usize);
            }
            self.skip = 0;
            let got = out.len() - before;
            let head = head.min(got);
            out.drain(before..before + head);
            let room = (self.file.len - self.pos) as usize;
            out.truncate(before + (out.len() - before).min(room));
            self.pos += (out.len() - before) as u64;
            if out.len() > before {
                return Ok(true);
            }
        }
    }

    fn decode_next(&mut self, out: &mut Vec<f32>) -> Result<()> {
        let pkt = &self.file.packets[self.next];
        self.next += 1;
        let before = out.len();
        out.resize(before + 5760, 0.0);
        let n = self
            .dec
            .decode_float(pkt, &mut out[before..], false)
            .context("Opus decode")?;
        out.truncate(before + n);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_length_and_timing() {
        let dir = std::env::temp_dir().join(format!("vox_opus_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.opus");
        // 1 s at 16 kHz: silence with a click at exactly 0.5 s.
        let mut input = vec![0.0f32; 16_000];
        for s in &mut input[8000..8016] {
            *s = 0.9;
        }
        let mut w = OpusWriter::create(&path, 16_000).unwrap();
        for chunk in input.chunks(333) {
            w.write(chunk).unwrap();
        }
        w.finish().unwrap();

        let f = OpusFile::open(&path).unwrap();
        // The resampler holds back its last couple of samples.
        assert!((f.len as i64 - 48_000).abs() <= 3, "{}", f.len);
        let mut c = f.cursor(0).unwrap();
        let mut out = Vec::new();
        while c.read(&mut out).unwrap() {}
        assert_eq!(out.len() as u64, f.len);
        let peak = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap()
            .0;
        assert!((peak as i64 - 24_000).abs() < 200, "click at {peak}");

        // Seeking lands on the same audio.
        let mut c = f.cursor(23_900).unwrap();
        let mut tail = Vec::new();
        while c.read(&mut tail).unwrap() {}
        assert_eq!(tail.len() as u64, f.len - 23_900);
        let peak2 = tail
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap()
            .0;
        assert!((peak2 as i64 + 23_900 - peak as i64).abs() < 10);
        std::fs::remove_dir_all(&dir).ok();
    }
}

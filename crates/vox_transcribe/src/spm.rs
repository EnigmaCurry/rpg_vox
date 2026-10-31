//! Writing a Marian model's `tokenizer.json` from the `source.spm` it
//! ships with, so the models can be downloaded like every other model
//! rather than converted by hand.
//!
//! [`crate::ct2`] explains why the tokenizer has to be a
//! `tokenizer.json` read by the pure-Rust `tokenizers` crate and not the
//! `.spm` read by SentencePiece: the SentencePiece library cannot be in
//! the same binary as sherpa-onnx. That is also why this module exists
//! rather than a call to `SentencePieceProcessor`. What it needs from
//! the file is small -- the pieces and their scores -- and a `.spm` is a
//! protobuf, so the fields are read off the wire directly:
//!
//! ```text
//! ModelProto.pieces           field 1, repeated, length-delimited
//!   SentencePiece.piece       field 1, length-delimited (UTF-8)
//!   SentencePiece.score       field 2, 32-bit float
//! ```
//!
//! Everything else in the file -- the trainer and normalizer specs, the
//! piece types -- is skipped, because the conversion does not use it:
//! the ids of `<unk>` and `</s>` are found by looking the pieces up by
//! name, which is the same answer `unk_id()` and `eos_id()` give for
//! these models and needs no field numbers from a proto this does not
//! otherwise read.
//!
//! The result is byte-for-byte what `tools/spm2tokenizer.py` writes,
//! which is the same `tokenizers` crate reached through its Python
//! bindings -- with a real SentencePiece beside it to check the split
//! against, which is the part no binary here can have. That is what
//! makes the script worth keeping, and
//! `tokenizer_json_is_what_the_python_conversion_wrote` below is how it
//! stays true: the bytes that script wrote for the Russian pair are
//! fingerprinted there.

use std::path::Path;

use anyhow::{bail, Context as _, Result};
use tokenizers::models::unigram::Unigram;
use tokenizers::pre_tokenizers::metaspace::{Metaspace, PrependScheme};
use tokenizers::processors::template::TemplateProcessing;
use tokenizers::Tokenizer;

/// What SentencePiece writes in place of a space.
const SPACE: char = '▁';

/// The unknown piece, and the one the template appends.
const UNK: &str = "<unk>";
const EOS: &str = "</s>";

/// Write `out` from `spm`, and return how many pieces it has.
///
/// The tokenizer splits text into exactly the pieces the `.spm` would:
/// CTranslate2 is handed the token *strings* and maps them to ids itself
/// from the model's `shared_vocabulary.json`, so that is the whole job.
pub fn write_tokenizer(spm: &Path, out: &Path) -> Result<usize> {
    let bytes = std::fs::read(spm).with_context(|| format!("read {}", spm.display()))?;
    let vocab = pieces(&bytes).with_context(|| format!("read the pieces in {}", spm.display()))?;
    let count = vocab.len();
    let find = |want: &str| vocab.iter().position(|(piece, _)| piece == want);
    let unk = find(UNK).with_context(|| format!("no {UNK} piece in {}", spm.display()))?;
    let eos = find(EOS).with_context(|| format!("no {EOS} piece in {}", spm.display()))?;

    let model = Unigram::from(vocab, Some(unk), false)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("build the Unigram model")?;
    let mut tokenizer = Tokenizer::new(model);
    // Standard SentencePiece: a space becomes ▁, and one is added at the
    // front, which is what "▁Yes" rather than "Yes" depends on.
    let metaspace = Metaspace::new(SPACE, PrependScheme::Always, true);
    tokenizer.with_pre_tokenizer(Some(metaspace.clone()));
    tokenizer.with_decoder(Some(metaspace));
    // Marian has no bos; the eos is appended, which is what the
    // sentencepiece path in ct2rs does by hand.
    let template = TemplateProcessing::builder()
        .try_single(format!("$A {EOS}"))
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .special_tokens(vec![(EOS, eos as u32)])
        .build()
        .context("build the eos template")?;
    tokenizer.with_post_processor(Some(template));

    // It is worth nothing at all if it doesn't split text; a vocabulary
    // read off the wrong field would still build a tokenizer, and this
    // is the cheapest thing that notices.
    let said = "Yes, extremely hot.";
    let encoded = tokenizer
        .encode(said, true)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("encode a sample line")?;
    let tokens = encoded.get_tokens();
    match tokens.last() {
        Some(last) if last == EOS && tokens.len() > 1 => {}
        _ => bail!(
            "the tokenizer built from {} split {said:?} into {tokens:?}",
            spm.display()
        ),
    }

    tokenizer
        .save(out, true)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("write {}", out.display()))?;
    Ok(count)
}

/// `(piece, score)` for every piece in a `.spm`, in id order.
fn pieces(mut bytes: &[u8]) -> Result<Vec<(String, f64)>> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let (field, wire) = tag(&mut bytes)?;
        // ModelProto.pieces.
        if field == 1 && wire == 2 {
            out.push(piece(delimited(&mut bytes)?)?);
        } else {
            skip(&mut bytes, wire)?;
        }
    }
    if out.is_empty() {
        bail!("no pieces");
    }
    Ok(out)
}

/// One `SentencePiece` submessage. A piece that says nothing about its
/// score has the proto2 default, which is the 0 the control pieces
/// (`<unk>`, `<s>`, `</s>`) carry anyway.
fn piece(mut bytes: &[u8]) -> Result<(String, f64)> {
    let mut text = None;
    let mut score = 0f32;
    while !bytes.is_empty() {
        let (field, wire) = tag(&mut bytes)?;
        match (field, wire) {
            (1, 2) => {
                let raw = delimited(&mut bytes)?;
                text = Some(
                    std::str::from_utf8(raw)
                        .context("a piece is not UTF-8")?
                        .to_string(),
                );
            }
            (2, 5) => score = f32::from_le_bytes(fixed::<4>(&mut bytes)?),
            _ => skip(&mut bytes, wire)?,
        }
    }
    // f32 widened, not reparsed: the file stores 32-bit floats and the
    // JSON holds 64-bit ones, and this is the conversion the Python
    // bindings make too, down to the last digit of `-2.717104196548462`.
    Ok((text.context("a piece with no text")?, f64::from(score)))
}

/// A field's number and wire type.
fn tag(bytes: &mut &[u8]) -> Result<(u64, u8)> {
    let key = varint(bytes)?;
    Ok((key >> 3, (key & 7) as u8))
}

fn varint(bytes: &mut &[u8]) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..10).map(|i| i * 7) {
        let (&byte, rest) = bytes.split_first().context("truncated varint")?;
        *bytes = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    bail!("varint longer than 10 bytes")
}

/// A length-delimited field's bytes, consumed from `bytes`.
fn delimited<'a>(bytes: &mut &'a [u8]) -> Result<&'a [u8]> {
    let len = usize::try_from(varint(bytes)?).context("length out of range")?;
    if bytes.len() < len {
        bail!("a length-delimited field runs past the end of the message");
    }
    let (head, rest) = bytes.split_at(len);
    *bytes = rest;
    Ok(head)
}

fn fixed<const N: usize>(bytes: &mut &[u8]) -> Result<[u8; N]> {
    if bytes.len() < N {
        bail!("a {N}-byte field runs past the end of the message");
    }
    let (head, rest) = bytes.split_at(N);
    *bytes = rest;
    Ok(head.try_into().expect("split_at gave N bytes"))
}

/// Step over a field this conversion does not read.
fn skip(bytes: &mut &[u8], wire: u8) -> Result<()> {
    match wire {
        0 => {
            varint(bytes)?;
        }
        1 => {
            fixed::<8>(bytes)?;
        }
        2 => {
            delimited(bytes)?;
        }
        5 => {
            fixed::<4>(bytes)?;
        }
        _ => bail!("wire type {wire} is not one a .spm has"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `SentencePiece` submessage: piece (field 1), score (field 2).
    fn encoded_piece(text: &str, score: f32) -> Vec<u8> {
        let mut out = vec![0x0a, text.len() as u8];
        out.extend_from_slice(text.as_bytes());
        out.push(0x15);
        out.extend_from_slice(&score.to_le_bytes());
        out
    }

    /// `ModelProto` holding `pieces`, plus a field this does not read.
    fn encoded_model(pieces: &[(&str, f32)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (text, score) in pieces {
            let body = encoded_piece(text, *score);
            out.push(0x0a);
            out.push(body.len() as u8);
            out.extend_from_slice(&body);
        }
        // trainer_spec (field 2), which is skipped over.
        out.extend_from_slice(&[0x12, 0x03, 0x08, 0x01, 0x00]);
        out
    }

    #[test]
    fn reads_the_pieces_and_scores_off_the_wire() {
        let got = pieces(&encoded_model(&[
            ("<unk>", 0.0),
            ("</s>", 0.0),
            ("▁the", -2.717_104_2),
        ]))
        .expect("parse");
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], ("<unk>".to_string(), 0.0));
        assert_eq!(got[2].0, "▁the");
        // The widening the JSON shows, rather than a rounded -2.7171042.
        assert_eq!(got[2].1, f64::from(-2.717_104_2f32));
    }

    #[test]
    fn refuses_a_file_that_is_not_a_model() {
        assert!(pieces(b"").is_err(), "nothing at all");
        assert!(pieces(&[0x0a, 0x7f]).is_err(), "a piece past the end");
    }

    /// The real conversion, against the bytes `tools/spm2tokenizer.py`
    /// wrote for the same `source.spm`: the length and FNV-1a of each,
    /// taken from that script's output. Comparing against whatever
    /// `tokenizer.json` is beside the model would not say much for long,
    /// since it is this code that writes that file now.
    ///
    /// Not run by default: it wants the models downloaded.
    ///
    /// ```text
    /// VOX_SCRIBE_MODELS=models cargo test -p vox_transcribe --features ct2 \
    ///   -- --ignored --nocapture python
    /// ```
    #[test]
    #[ignore = "converts a downloaded model's source.spm"]
    fn tokenizer_json_is_what_the_python_conversion_wrote() {
        let dir = std::env::var("VOX_SCRIBE_MODELS").expect("VOX_SCRIBE_MODELS");
        let translate = Path::new(&dir).join(crate::ct2::TRANSLATE);
        let mut checked = 0;
        for (pair, len, fnv) in [
            ("en-ru", 2_092_900usize, 0xa7cd_2e81_6e1c_7054u64),
            ("ru-en", 2_370_411, 0x8e72_4e2d_8568_d91e),
        ] {
            let model = translate.join(pair);
            let spm = model.join("source.spm");
            if !spm.is_file() {
                println!("{}: not downloaded, skipped", model.display());
                continue;
            }
            let out = std::env::temp_dir().join(format!("vox_transcribe-{pair}-tokenizer.json"));
            let count = write_tokenizer(&spm, &out).expect("convert");
            let got = std::fs::read(&out).expect("read what was written");
            assert_eq!(got.len(), len, "{pair}: {} bytes, not {len}", got.len());
            assert_eq!(fnv1a(&got), fnv, "{pair}: {len} bytes but different ones");
            println!("{pair}: {count} pieces, the bytes the Python conversion wrote");
            let _ = std::fs::remove_file(&out);
            checked += 1;
        }
        assert!(checked > 0, "no downloaded model to convert");
    }

    fn fnv1a(bytes: &[u8]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        hash
    }
}

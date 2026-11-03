//! Voices cloned from somebody in the room: a few seconds of their own
//! speech in, a voice one of the AIs can be asked to speak in out.
//!
//! The server is the `clone` container of the `qwen3-tts/` stack in this
//! repo -- vllm-omni holding Qwen3-TTS's Base model, which is the one
//! that can take a reference clip at all. It is a *different server*
//! from the one the preset voices come from, on a hostname of its own,
//! and that is why this is its own URL rather than a flag on the other:
//! asking the presets server for a cloned voice gets a preset, and
//! nobody would hear the difference until they heard it.
//!
//! Three things are asked of it, and none of them is speaking:
//!
//! * `POST /v1/audio/voices` -- a WAV and a name, as multipart. The
//!   server keeps the clip under its own `SPEAKER_SAMPLES_DIR` and
//!   pulls the speaker embedding off it the first time the voice is
//!   used.
//! * `GET /v1/audio/voices` -- the names it is holding, which is how
//!   the room can tell whether a voice it wrote down is still there.
//! * `DELETE /v1/audio/voices/{name}` -- forget one.
//!
//! Speaking in a voice that was uploaded here is [`crate::tts::Qwen3`]'s
//! half: the same `/v1/audio/speech` every voice is asked through, with
//! `task_type: "Base"` and the name instead of a preset. See
//! [`crate::tts::Borrowed`].

use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;

/// The longest a cloned voice's name may be. Short because it is said
/// out loud to ask for it, and because it ends up a filename on the
/// server.
const NAME_MAX: usize = 48;

/// A voice-cloning server, as HTTP and nothing else. Cheap to make and
/// cheap to clone: it is a connection pool and two strings.
#[derive(Clone)]
pub struct Clones {
    url: String,
    /// Bearer token, for a server that is not wide open.
    key: Option<String>,
    http: ureq::Agent,
}

/// What the server says it kept, which is a name. Everything else it
/// knows about a voice -- whose it is, what they read -- the room
/// keeps its own row for.
#[derive(Debug, Clone, Deserialize)]
struct Uploaded {
    name: String,
}

impl Clones {
    pub fn new(url: &str, key: Option<String>) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            key,
            // Two different waits. Reading is long, because this is an
            // upload and the server extracts an embedding off the back
            // of it. Connecting is short: a host the container cannot
            // reach is the one failure this is actually likely to hit
            // -- split-horizon DNS, a name that resolves to an address
            // on the wrong side of the room -- and ureq would spend
            // thirty seconds on it by default, which is long enough
            // that the turn is over before the model is told.
            http: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(5))
                .timeout_read(Duration::from_secs(120))
                .timeout_write(Duration::from_secs(120))
                .build(),
        }
    }

    /// Keep this clip under this name. `consent` is the server's own
    /// field and it is required: what goes in it is who the clip is of
    /// and which room they read it in, because that is the whole of
    /// what this house knows about why it is allowed to be here.
    ///
    /// `ref_text` is what the clip says, which the server uses to clone
    /// in context and which makes the voice markedly better. The room
    /// has it without asking: it is the transcript of the words it just
    /// recorded.
    pub fn upload(
        &self,
        name: &str,
        wav: &[u8],
        ref_text: &str,
        consent: &str,
        description: &str,
    ) -> Result<()> {
        let mut parts: Vec<Part> = vec![
            Part::text("name", name),
            Part::text("consent", consent),
            Part::text("ref_text", ref_text),
            Part::file("audio_sample", "sample.wav", "audio/wav", wav),
        ];
        if !description.trim().is_empty() {
            parts.push(Part::text("speaker_description", description.trim()));
        }
        let (content_type, body) = multipart(&parts);
        let url = format!("{}/v1/audio/voices", self.url);
        let mut req = self.http.post(&url).set("Content-Type", &content_type);
        if let Some(key) = &self.key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        let said = match req.send_bytes(&body) {
            Ok(r) => r.into_string().unwrap_or_default(),
            Err(ureq::Error::Status(code, r)) => {
                let detail = r.into_string().unwrap_or_default();
                bail!("{url}: HTTP {code}: {}", clip(&detail));
            }
            Err(e) => return Err(e).with_context(|| format!("POST {url}")),
        };
        // `{"success": true, "voice": {"name": …}}`. The name is
        // checked back because a server that renamed what it was given
        // would leave the room with a row for a voice nothing can be
        // spoken in, and no way to tell.
        #[derive(Deserialize)]
        struct Envelope {
            #[serde(default)]
            success: bool,
            #[serde(default)]
            voice: Option<Uploaded>,
            #[serde(default)]
            error: Option<serde_json::Value>,
        }
        let env: Envelope = serde_json::from_str(&said)
            .with_context(|| format!("{url} answered with {}", clip(&said)))?;
        if !env.success {
            bail!("{url} refused it: {:?}", env.error);
        }
        match env.voice {
            Some(v) if v.name == name => Ok(()),
            Some(v) => bail!("the server called it {:?} and not {name:?}", v.name),
            None => bail!("{url} said it worked without saying what it kept"),
        }
    }

    /// The names the server is holding, uploaded ones and its own.
    pub fn list(&self) -> Result<Vec<String>> {
        let url = format!("{}/v1/audio/voices", self.url);
        let mut req = self.http.get(&url);
        if let Some(key) = &self.key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        let said = match req.call() {
            Ok(r) => r.into_string().context("read the voices")?,
            Err(ureq::Error::Status(code, r)) => {
                let detail = r.into_string().unwrap_or_default();
                bail!("{url}: HTTP {code}: {}", clip(&detail));
            }
            Err(e) => return Err(e).with_context(|| format!("GET {url}")),
        };
        #[derive(Deserialize)]
        struct Listed {
            #[serde(default)]
            voices: Vec<String>,
        }
        let listed: Listed = serde_json::from_str(&said)
            .with_context(|| format!("{url} answered with {}", clip(&said)))?;
        Ok(listed.voices)
    }

    /// Forget one. A name the server has never heard of is not an
    /// error worth passing on: the point of asking was that it should
    /// be gone, and it is.
    pub fn forget(&self, name: &str) -> Result<()> {
        let url = format!("{}/v1/audio/voices/{}", self.url, urlencode(name));
        let mut req = self.http.delete(&url);
        if let Some(key) = &self.key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        match req.call() {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(404, _)) => Ok(()),
            Err(ureq::Error::Status(code, r)) => {
                let detail = r.into_string().unwrap_or_default();
                bail!("{url}: HTTP {code}: {}", clip(&detail));
            }
            Err(e) => Err(e).with_context(|| format!("DELETE {url}")),
        }
    }
}

/// The name a voice is kept under on the server, from the name it is
/// called by out loud. None when there is nothing left of it: a name
/// that is all punctuation is not a name.
///
/// Spelled down to letters, digits and dashes because it becomes a
/// filename on a server that is not ours and a path in a URL on the
/// way there. What people say and see is the name they gave; this is
/// the room's business with the server, and the two are kept apart the
/// same way a room's id is kept apart from what it is called.
pub fn id_for(name: &str) -> Option<String> {
    let mut id = String::new();
    for c in name.trim().chars() {
        match c {
            c if c.is_ascii_alphanumeric() => id.push(c.to_ascii_lowercase()),
            ' ' | '-' | '_' | '.' if !id.is_empty() && !id.ends_with('-') => id.push('-'),
            // A space before any of it, or a second one running: not a
            // dash, because a name does not start with one and does
            // not have two in a row.
            ' ' | '-' | '_' | '.' => {}
            // Anything else -- an accent, a comma, an emoji -- is not
            // spelled at all rather than guessed at.
            _ => {}
        }
        if id.len() >= NAME_MAX {
            break;
        }
    }
    let id = id.trim_end_matches('-').to_string();
    (!id.is_empty()).then_some(id)
}

/// A clip as a WAV the clone server will take: 16-bit mono at
/// `rate`, which is the one thing every part of this understands.
pub fn wav(samples: &[f32], rate: u32) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut bytes = Vec::with_capacity(samples.len() * 2 + 44);
    {
        let mut w =
            hound::WavWriter::new(std::io::Cursor::new(&mut bytes), spec).context("write a WAV")?;
        for s in samples {
            w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
        w.finalize().context("finish a WAV")?;
    }
    Ok(bytes)
}

/// One field of a multipart body.
struct Part {
    name: String,
    /// Set for a file, which is what gives it a filename and a type.
    filename: Option<String>,
    content_type: Option<String>,
    bytes: Vec<u8>,
}

impl Part {
    fn text(name: &str, value: &str) -> Self {
        Self {
            name: name.to_string(),
            filename: None,
            content_type: None,
            bytes: value.as_bytes().to_vec(),
        }
    }

    fn file(name: &str, filename: &str, content_type: &str, bytes: &[u8]) -> Self {
        Self {
            name: name.to_string(),
            filename: Some(filename.to_string()),
            content_type: Some(content_type.to_string()),
            bytes: bytes.to_vec(),
        }
    }
}

/// A `multipart/form-data` body and the header that describes it.
///
/// Written out here because `ureq` doesn't do multipart and this is the
/// one request in the whole room that needs it.
///
/// The boundary is fixed, and one of the fields is a transcript of
/// something somebody said out loud -- so the boundary is taken out of
/// every text field on the way in rather than assumed not to be there.
/// Nobody is going to say it, and a body that could be broken by
/// somebody saying a particular string is a body that can be broken.
fn multipart(parts: &[Part]) -> (String, Vec<u8>) {
    let boundary = "----vox-room-voice-sample";
    let mut body = Vec::new();
    for p in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        let mut disposition = format!("Content-Disposition: form-data; name=\"{}\"", p.name);
        if let Some(f) = &p.filename {
            disposition.push_str(&format!("; filename=\"{f}\""));
        }
        body.extend_from_slice(disposition.as_bytes());
        body.extend_from_slice(b"\r\n");
        if let Some(t) = &p.content_type {
            body.extend_from_slice(format!("Content-Type: {t}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        match &p.filename {
            // A file is bytes and is not read for the boundary: the
            // length of the part is not what delimits it, but a WAV
            // that happened to contain the boundary would have to
            // contain it as a line of its own, which a WAV cannot.
            Some(_) => body.extend_from_slice(&p.bytes),
            None => body.extend_from_slice(
                String::from_utf8_lossy(&p.bytes)
                    .replace(boundary, "")
                    .as_bytes(),
            ),
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// A name in a URL path. Ids are letters, digits and dashes by
/// construction, so this is the belt to that braces: a name that came
/// back off a server is not ours.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// What a server said, as much of it as is worth repeating.
fn clip(s: &str) -> String {
    s.chars().take(300).collect()
}

/// A WAV's samples and rate, for a test or anything else that wants to
/// read back what was written.
#[cfg(test)]
pub fn read_wav(bytes: &[u8]) -> Result<(Vec<f32>, u32)> {
    let mut r = hound::WavReader::new(std::io::Cursor::new(bytes))?;
    let spec = r.spec();
    let samples: Vec<f32> = r
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / i16::MAX as f32))
        .collect::<std::result::Result<_, _>>()?;
    Ok((samples, spec.sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spells_a_name_a_server_can_keep() {
        assert_eq!(id_for("Ryan").as_deref(), Some("ryan"));
        assert_eq!(id_for("Vortex Prime").as_deref(), Some("vortex-prime"));
        assert_eq!(
            id_for("  the Old Sailor  ").as_deref(),
            Some("the-old-sailor")
        );
        assert_eq!(id_for("Ryan's voice!").as_deref(), Some("ryans-voice"));
        // Nothing left of it: not a name.
        assert_eq!(id_for("???"), None);
        assert_eq!(id_for(""), None);
        // Long enough to be a filename and no longer.
        let long = id_for(&"a".repeat(200)).unwrap();
        assert!(long.len() <= NAME_MAX, "{} chars", long.len());
    }

    #[test]
    fn writes_a_wav_the_server_will_take() {
        let tone: Vec<f32> = (0..24_000)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 24_000.0).sin() * 0.5)
            .collect();
        let bytes = wav(&tone, 24_000).unwrap();
        let (back, rate) = read_wav(&bytes).unwrap();
        assert_eq!(rate, 24_000);
        assert_eq!(back.len(), tone.len());
        assert!((back[6_000] - tone[6_000]).abs() < 0.001);
    }

    #[test]
    fn a_multipart_body_has_every_field_in_it() {
        let (content_type, body) = multipart(&[
            Part::text("name", "ryan"),
            Part::file("audio_sample", "sample.wav", "audio/wav", b"RIFFxxxx"),
        ]);
        assert!(content_type.starts_with("multipart/form-data; boundary="));
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("name=\"name\"\r\n\r\nryan"), "{text}");
        assert!(
            text.contains("name=\"audio_sample\"; filename=\"sample.wav\""),
            "{text}"
        );
        assert!(text.contains("Content-Type: audio/wav"), "{text}");
        assert!(text.trim_end().ends_with("--"), "{text}");
    }

    /// The real server, end to end: a second of a tone kept under a
    /// name, listed, spoken in, and forgotten again. Not run by
    /// default.
    ///
    /// ```text
    /// VOX_ROOM_TTS_CLONE_URL=… cargo test -p vox_room -- --ignored --nocapture clones
    /// ```
    #[test]
    #[ignore = "uploads a voice to the configured clone server"]
    fn clones_a_voice_on_the_real_server() {
        let url = std::env::var("VOX_ROOM_TTS_CLONE_URL").expect("VOX_ROOM_TTS_CLONE_URL");
        let key = std::env::var("VOX_ROOM_TTS_KEY")
            .ok()
            .filter(|k| !k.is_empty());
        let clones = Clones::new(&url, key.clone());
        let name = "vox-room-self-test";
        // Not speech, which is the point: what is being tested here is
        // the three requests, not the quality of a clone.
        let tone: Vec<f32> = (0..24_000 * 4)
            .map(|i| (i as f32 * 180.0 * std::f32::consts::TAU / 24_000.0).sin() * 0.3)
            .collect();
        let bytes = wav(&tone, 24_000).unwrap();
        clones
            .upload(
                name,
                &bytes,
                "this is a test recording",
                "vox_room:self-test",
                "",
            )
            .expect("upload");
        let listed = clones.list().expect("list");
        assert!(listed.iter().any(|v| v == name), "{listed:?}");
        // And it can be spoken in, which is the other half of the
        // feature and a different server call.
        let mut voice = crate::tts::Qwen3::new(&url, "", "", "English", key, None).cloning(
            Some(url.clone()),
            std::sync::Arc::new(std::sync::Mutex::new(Some(name.to_string()))),
        );
        let samples = crate::tts::Voice::speak(&mut voice, "The dragon was very old.", None)
            .expect("speak in the cloned voice");
        let secs = samples.len() as f32 / crate::mixer::RATE as f32;
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        println!("spoke {secs:.2}s, peak {peak:.3}");
        assert!(secs > 0.3 && peak > 0.01, "{secs:.2}s, peak {peak:.3}");
        clones.forget(name).expect("forget");
        let listed = clones.list().expect("list again");
        assert!(!listed.iter().any(|v| v == name), "{listed:?}");
    }
}

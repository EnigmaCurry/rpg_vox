//! The AI's voice: a sentence of text in, 48 kHz mono samples out.
//!
//! * `qwen3`: a Qwen3-TTS server speaking vllm-omni's `/v1/audio/speech`
//!   (the `qwen3-tts/` stack in this repo, the same one `pi-tts.py` uses).
//! * `kokoro`: the local Kokoro model scribe can download, for when no
//!   server is around.
//! * `none`: silence as long as the sentence would take to read, so the
//!   room can be tried without any voice at all.
//!
//! A Qwen3 voice can also be *lent* a voice cloned from somebody in the
//! room, which is a sentence asked of a different server in a different
//! shape -- see [`Borrowed`] and [`crate::voices`].

use std::io::Read as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use rubato::{
    Resampler as _, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

use crate::mixer::RATE;

/// The voices a Qwen3-TTS server actually holds, by the names its
/// `/v1/audio/speech` will take.
///
/// Asked once, as the house opens, because the answer is the server's
/// and not ours to guess. The names are not what anybody would write
/// down from a list of voices: the one spelled "Uncle Fu" in a picker
/// is `uncle_fu` here, and a name the endpoint does not know is an
/// HTTP 400 in the middle of a sentence somebody was waiting to hear.
///
/// What comes back is filtered to the presets. The same endpoint
/// reports every voice cloned onto that server as well -- uuids, and
/// whatever a test left behind -- and those are nobody's character to
/// build on: they are somebody's voice, reached by name through
/// [`Borrowed`]. `default` goes too, which is the server saying "the
/// one you get when you don't ask" rather than naming a voice.
pub fn presets(url: &str, key: Option<&str>) -> Result<Vec<String>> {
    let url = format!("{}/v1/audio/voices", url.trim_end_matches('/'));
    let http = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build();
    let mut req = http.get(&url);
    if let Some(key) = key {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    let said = match req.call() {
        Ok(r) => r.into_string().context("read the voices")?,
        Err(ureq::Error::Status(code, r)) => {
            let detail = r.into_string().unwrap_or_default();
            bail!(
                "{url}: HTTP {code}: {}",
                detail.chars().take(200).collect::<String>()
            );
        }
        Err(e) => return Err(e).with_context(|| format!("GET {url}")),
    };
    #[derive(serde::Deserialize)]
    struct Uploaded {
        #[serde(default)]
        name: String,
    }
    #[derive(serde::Deserialize)]
    struct Listed {
        #[serde(default)]
        voices: Vec<String>,
        /// The ones cloned onto it, which the server tells us apart
        /// from its own -- so the filtering is its answer and not a
        /// guess about which names look like uuids.
        #[serde(default)]
        uploaded_voices: Vec<Uploaded>,
    }
    let listed: Listed = serde_json::from_str(&said).with_context(|| {
        format!(
            "{url} answered with {}",
            said.chars().take(200).collect::<String>()
        )
    })?;
    let cloned: Vec<&str> = listed
        .uploaded_voices
        .iter()
        .map(|u| u.name.as_str())
        .collect();
    Ok(listed
        .voices
        .into_iter()
        .filter(|v| v != "default" && !cloned.contains(&v.as_str()))
        .collect())
}

pub trait Voice: Send {
    /// `language` overrides the voice's own, for a room that is listening
    /// in more than one. Spelled out in English, as Qwen3 wants it.
    fn speak(&mut self, text: &str, language: Option<&str>) -> Result<Vec<f32>>;
}

/// A voice one of the AIs has been lent instead of its own: the name a
/// cloned voice is kept under on the clone server, or None for the
/// preset it was started with.
///
/// Shared, because a bot has one voice per language it speaks in and
/// they are each on a thread of their own: being lent a voice is the
/// bot's, not the thread's. The room sets it (see `Room::speak_as`) and
/// every one of its voices reads it on the next sentence it is asked
/// for -- so a voice changes where a sentence ends, and never inside
/// one. Nothing here is written down: the voice it keeps is written
/// down, and which voice it is speaking in today is not, so a restart
/// is a bot in its own voice again.
pub type Borrowed = Arc<Mutex<Option<String>>>;

/// One voice several rooms speak through, in turn.
///
/// A Qwen3 voice is only an HTTP client, so a room that wants one makes
/// its own and they talk at the same time. Kokoro is the whole model --
/// hundreds of megabytes on the CPU -- and loading it again for every
/// room somebody opens is not on, so every room shares the one copy.
/// They queue behind each other, which costs nothing real: the model
/// is one lot of CPU however many rooms are asking it to read.
#[derive(Clone)]
pub struct Shared(Arc<Mutex<Box<dyn Voice>>>);

impl Shared {
    pub fn new(voice: Box<dyn Voice>) -> Self {
        Self(Arc::new(Mutex::new(voice)))
    }

    /// Another way in to the same voice, for another room.
    pub fn handle(&self) -> Box<dyn Voice> {
        Box::new(self.clone())
    }
}

impl Voice for Shared {
    fn speak(&mut self, text: &str, language: Option<&str>) -> Result<Vec<f32>> {
        self.0
            .lock()
            .expect("voice mutex poisoned")
            .speak(text, language)
    }
}

pub struct Qwen3 {
    url: String,
    voice: String,
    instruct: String,
    language: String,
    /// Bearer token, for a server that is not wide open.
    key: Option<String>,
    /// Named in the request only when the server serves more than one.
    model: Option<String>,
    /// The voice-cloning server, where there is one: a different host
    /// holding a different model, because the preset server cannot
    /// speak in a voice somebody uploaded. See [`crate::voices`].
    clone_url: Option<String>,
    /// A voice this one has been lent, when it has.
    borrowed: Borrowed,
    http: ureq::Agent,
}

impl Qwen3 {
    pub fn new(
        url: &str,
        voice: &str,
        instruct: &str,
        language: &str,
        key: Option<String>,
        model: Option<String>,
    ) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            voice: voice.to_string(),
            instruct: instruct.to_string(),
            language: language.to_string(),
            key,
            model,
            clone_url: None,
            borrowed: Borrowed::default(),
            http: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(60))
                .build(),
        }
    }

    /// Where a cloned voice would come from, and the cell that says
    /// whether this voice is being lent one. Both or neither: a cell
    /// that can be set with no server to ask is a voice that would go
    /// silent the moment somebody borrowed it.
    pub fn cloning(mut self, clone_url: Option<String>, borrowed: Borrowed) -> Self {
        if let Some(url) = clone_url {
            self.clone_url = Some(url.trim_end_matches('/').to_string());
            self.borrowed = borrowed;
        }
        self
    }
}

impl Voice for Qwen3 {
    fn speak(&mut self, text: &str, language: Option<&str>) -> Result<Vec<f32>> {
        let language = language.unwrap_or(&self.language);
        // A voice it has been lent, if it has one and there is a server
        // to ask for it. Read per sentence rather than held: what the
        // room set between two sentences takes effect at the second,
        // which is as soon as anybody could hear it anyway.
        let borrowed = match &self.clone_url {
            Some(url) => self
                .borrowed
                .lock()
                .expect("borrowed voice mutex poisoned")
                .clone()
                .map(|name| (url.clone(), name)),
            None => None,
        };
        // Somebody's own voice, off the clone server, and then its own
        // again if that fails. `task_type` has to be said: the Base
        // model is the only one that can speak in an uploaded voice,
        // and it is also the one that does something else entirely
        // when it isn't told. No `instructions` -- a cloned voice is
        // how somebody sounds, and a style on top of it is a style on
        // top of them.
        //
        // The fallback is not politeness. A voice can be deleted from
        // under a bot that is wearing it -- another room's AI was lent
        // the same one, the server was rebuilt -- and a bot that went
        // silent for the rest of the conversation because of it would
        // be a worse answer than a bot that sounds like itself again.
        if let Some((url, name)) = borrowed {
            let body = serde_json::json!({
                "input": text,
                "voice": name,
                "task_type": "Base",
                "language": language,
                "response_format": "wav",
            });
            match self.ask(&url, body) {
                Ok(samples) => return Ok(samples),
                Err(err) => tracing::warn!(
                    voice = %name,
                    "the cloned voice didn't answer, so this is in its own: {err:#}"
                ),
            }
        }
        let mut body = serde_json::json!({
            "input": text,
            "voice": self.voice,
            "language": language,
            "response_format": "wav",
        });
        if !self.instruct.is_empty() {
            body["instructions"] = self.instruct.clone().into();
        }
        // Only ever the preset server's: the clone server is a server
        // of its own with one model on it, and naming the other one at
        // it is naming a model it hasn't got.
        if let Some(model) = &self.model {
            body["model"] = model.clone().into();
        }
        let url = self.url.clone();
        self.ask(&url, body)
    }
}

impl Qwen3 {
    /// One sentence asked of one server, as room samples.
    fn ask(&self, base: &str, body: serde_json::Value) -> Result<Vec<f32>> {
        let url = format!("{base}/v1/audio/speech");
        let mut req = self.http.post(&url);
        if let Some(key) = &self.key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        let resp = match req.send_json(body) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let detail = r.into_string().unwrap_or_default();
                bail!(
                    "{url}: HTTP {code}: {}",
                    detail.chars().take(300).collect::<String>()
                );
            }
            Err(e) => return Err(e).with_context(|| format!("POST {url}")),
        };
        let mut wav = Vec::new();
        resp.into_reader()
            .read_to_end(&mut wav)
            .context("read TTS audio")?;
        let (samples, rate) = decode_wav(&wav).context("TTS sent no WAV")?;
        to_room_rate(&samples, rate)
    }
}

pub struct Kokoro {
    tts: vox_transcribe::tts::Kokoro,
    sid: i32,
}

impl Kokoro {
    pub fn load(dir: &Path, voice: &str) -> Result<Self> {
        let sid = vox_transcribe::tts::voice_id(voice)
            .with_context(|| format!("no Kokoro voice {voice:?}"))?;
        Ok(Self {
            tts: vox_transcribe::tts::Kokoro::load(dir, 2)?,
            sid,
        })
    }
}

impl Voice for Kokoro {
    // Language comes from the voice id, and the bundle scribe downloads
    // is trimmed to English, so an override has nothing to offer here.
    fn speak(&mut self, text: &str, _language: Option<&str>) -> Result<Vec<f32>> {
        let samples = self.tts.speak(text, self.sid, 1.0)?;
        to_room_rate(&samples, self.tts.sample_rate())
    }
}

/// Silence for as long as reading the text aloud would take.
pub struct Silent;

/// Characters per second of speech, for [`Silent`].
const READING_CPS: f32 = 15.0;

impl Voice for Silent {
    fn speak(&mut self, text: &str, _language: Option<&str>) -> Result<Vec<f32>> {
        let secs = (text.chars().count() as f32 / READING_CPS).max(0.3);
        Ok(vec![0.0; (secs * RATE as f32) as usize])
    }
}

/// A WAV file as mono f32 and its sample rate. Several channels are
/// mixed down: what everything here wants is one.
///
/// Public because a WAV reaches this house two ways -- back from the
/// speech server, and in from somebody's computer as a clip to clone a
/// voice from -- and they want the same twenty lines. Each adds its
/// own context: a WAV that will not read means something different
/// depending on who sent it.
pub fn decode_wav(bytes: &[u8]) -> Result<(Vec<f32>, u32)> {
    let mut r = hound::WavReader::new(std::io::Cursor::new(bytes)).context("that is not a WAV")?;
    let spec = r.spec();
    let channels = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<std::result::Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<std::result::Result<_, _>>()?
        }
    };
    let mono = interleaved
        .chunks(channels)
        .map(|c| c.iter().sum::<f32>() / channels as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}

fn to_room_rate(samples: &[f32], rate: u32) -> Result<Vec<f32>> {
    resample(samples, rate, RATE)
}

/// `samples` at `from` Hz, at `to` Hz instead. The room's own rate is
/// what a voice is resampled to on the way in; a clip of somebody's
/// speech on the way out to the clone server is resampled the other
/// way, which is the only reason this takes both ends.
pub fn resample(samples: &[f32], from: u32, to: u32) -> Result<Vec<f32>> {
    if from == to || samples.is_empty() {
        return Ok(samples.to_vec());
    }
    let params = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    let ratio = to as f64 / from as f64;
    let chunk = 1024;
    let mut rs = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk, 1)?;
    let mut out = Vec::with_capacity((samples.len() as f64 * ratio) as usize + chunk * 4);
    let mut pos = 0;
    while pos + chunk <= samples.len() {
        out.extend_from_slice(&rs.process(&[&samples[pos..pos + chunk]], None)?[0]);
        pos += chunk;
    }
    // rubato refuses an empty partial chunk, so a length that is a whole
    // number of chunks skips straight to the flush.
    if pos < samples.len() {
        out.extend_from_slice(&rs.process_partial(Some(&[&samples[pos..]]), None)?[0]);
    }
    // Flush the filter's delay so the last word isn't clipped.
    out.extend_from_slice(&rs.process_partial::<&[f32]>(None, None)?[0]);
    let delay = (rs.output_delay()).min(out.len());
    out.drain(..delay);
    out.truncate((samples.len() as f64 * ratio).round() as usize);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_tts_audio_to_the_room_rate() {
        let tone: Vec<f32> = (0..24_000)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 24_000.0).sin() * 0.5)
            .collect();
        let out = to_room_rate(&tone, 24_000).unwrap();
        assert_eq!(out.len(), 48_000);
        let peak = out[1000..47_000].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.02, "peak {peak}");
    }

    #[test]
    fn resamples_any_length() {
        for n in [1, 100, 1023, 1024, 1025, 2048, 48_000] {
            let out = to_room_rate(&vec![0.1; n], 24_000).unwrap();
            assert_eq!(out.len(), n * 2, "{n} samples");
        }
    }

    #[test]
    fn reads_a_wav() {
        let mut bytes = Vec::new();
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 24_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::new(std::io::Cursor::new(&mut bytes), spec).unwrap();
            for _ in 0..100 {
                w.write_sample(16384i16).unwrap();
            }
            w.finalize().unwrap();
        }
        let (s, rate) = decode_wav(&bytes).unwrap();
        assert_eq!(rate, 24_000);
        assert_eq!(s.len(), 100);
        assert!((s[0] - 0.5).abs() < 1e-4);
    }

    /// The voice a translated room needs, through the real server: one
    /// sentence asked for in two languages, which is the per-call
    /// override the voice threads rely on. Not run by default.
    ///
    /// ```text
    /// VOX_ROOM_TTS_URL=… cargo test -p vox_room -- --ignored --nocapture speaks_two
    /// ```
    #[test]
    #[ignore = "asks the configured Qwen3-TTS server for two languages"]
    fn speaks_two_languages_from_one_voice() {
        let url = std::env::var("VOX_ROOM_TTS_URL")
            .or_else(|_| std::env::var("PI_TTS_URL"))
            .expect("VOX_ROOM_TTS_URL");
        let voice = std::env::var("VOX_ROOM_VOICE").unwrap_or_else(|_| "aiden".into());
        let key = std::env::var("VOX_ROOM_TTS_KEY")
            .ok()
            .filter(|k| !k.is_empty());
        let model = std::env::var("VOX_ROOM_TTS_MODEL")
            .ok()
            .filter(|m| !m.is_empty());
        let mut qwen3 = Qwen3::new(&url, &voice, "", "English", key, model);

        // Its own language, as every room has always asked for it.
        let english = qwen3
            .speak("The dragon was very old.", None)
            .expect("English");
        // And another, asked for per call: the same voice, the same
        // server, one word in the body different.
        let russian = qwen3
            .speak("Дракон был очень старым.", Some("Russian"))
            .expect("Russian");
        for (language, samples) in [("English", &english), ("Russian", &russian)] {
            let secs = samples.len() as f32 / RATE as f32;
            let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            println!("{language}: {secs:.2}s at {RATE} Hz, peak {peak:.3}");
            assert!(secs > 0.3, "{language} came back {secs:.2}s long");
            assert!(peak > 0.01, "{language} came back silent");
        }
    }

    /// The voices a server says it has, which are the only names its
    /// `/v1/audio/speech` will take.
    ///
    /// The body here is the real one, from the server this was written
    /// against. The names are nothing like what a picker shows -- the
    /// voice everybody calls "Uncle Fu" is `uncle_fu` -- and a list
    /// written down from the display names reached the server untouched
    /// and came back HTTP 400 in the middle of a line somebody was
    /// waiting to hear. So it is asked, and the answer is filtered to
    /// the voices a character can actually be built on: not the clones
    /// somebody uploaded, which are a person's voice and reached by
    /// name another way, and not `default`, which is the server saying
    /// "whatever you get when you don't ask".
    #[test]
    fn the_server_says_which_voices_are_its_own() {
        use std::io::{Read as _, Write as _};
        let body = r#"{"voices":["28fe97f2-990b-4acf-9224-d48ed4a4088e","aiden","default","dylan","eric","ono_anna","ryan","serena","smoke_test_clone","sohee","uncle_fu","vivian"],"uploaded_voices":[{"name":"28fe97f2-990b-4acf-9224-d48ed4a4088e","consent":"rpg_vox:x"},{"name":"smoke_test_clone","consent":"rpg_vox:y"}]}"#
            .replace("\\\n", "");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let served = body.clone();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("a request");
            let mut buf = [0u8; 2048];
            let read = stream.read(&mut buf).unwrap_or(0);
            let asked = String::from_utf8_lossy(&buf[..read]).to_string();
            assert!(asked.starts_with("GET /v1/audio/voices"), "{asked}");
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                served.len(),
                served
            );
        });

        let have = presets(&url, None).expect("the voices");
        assert_eq!(
            have,
            [
                "aiden", "dylan", "eric", "ono_anna", "ryan", "serena", "sohee", "uncle_fu",
                "vivian"
            ],
            "its own voices, and not the clones or `default`"
        );
    }

    #[test]
    fn silence_lasts_as_long_as_reading() {
        let s = Silent.speak("fifteen chars!!", None).unwrap();
        assert_eq!(s.len(), RATE as usize);
    }
}

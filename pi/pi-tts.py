"""Speak text for Pi with Qwen3-TTS, driven over JSONL.

Two ways to run the model: locally with mlx-audio (macOS, Apple Silicon),
or on a Qwen3-TTS server speaking vllm-omni's /v1/audio/speech (rpg_vox's
qwen3-tts/ stack, CustomVoice model), set by PI_TTS_URL. The server works
from any machine, Linux included.

stdin, one JSON object per line:
  {"type": "say", "text": "...", "id": 7}   queue a sentence to speak
  {"type": "stop"}                            drop everything queued, go quiet now
  {"type": "voices"}                          list the model's voices
  {"type": "set_voice", "voice": "ryan", "instruct": "Warm and playful."}
                                              speak with another voice from the next sentence;
                                              `instruct` (optional) is how to deliver it, "" for plainly
  {"type": "quit"}                            exit

stdout, one JSON object per line:
  {"type": "status", "message": "..."}
  {"type": "ready", "model": "...", "sample_rate_hz": 24000}
  {"type": "speaking"}                        audio started from quiet
  {"type": "progress", "id": 7, "played_s": 1.2, "total_s": 2.9}
                                              every ~100 ms while sentence 7 plays;
                                              `total_s` is null until it is all generated
  {"type": "said", "id": 7}                   sentence 7 has been played to the end
  {"type": "idle", "last_id": 7}              everything up to `last_id` has been played
  {"type": "voices", "voices": [{"name": "aiden", "native": "English"}, ...], "current": "aiden",
   "instruct": ""}
  {"type": "voice", "voice": "ryan", "instruct": "..."}   the voice is now this one
  {"type": "error", "message": "...", "fatal": false}

Logs go to stderr. Runs in the venv `just pi-tts-venv` makes.

Environment: PI_TTS_URL (a Qwen3-TTS server; without it, mlx-audio),
PI_TTS_MODEL (mlx), PI_TTS_VOICE (Aiden), PI_TTS_LANG (English),
PI_TTS_INSTRUCT (delivery instruction for every sentence; empty for none),
PI_TTS_STREAM_INTERVAL (0.32, mlx).
"""

import json
import os
import queue
import sys
import threading
import time
from collections import deque

# stdout is the protocol's: keep the real one, send anything else printed
# (library banners included) to stderr.
_out = os.fdopen(os.dup(1), "w", buffering=1)
os.dup2(2, 1)
_out_lock = threading.Lock()


def emit(event):
    with _out_lock:
        try:
            _out.write(json.dumps(event) + "\n")
            _out.flush()
        except BrokenPipeError:
            os._exit(1)


MODEL = os.environ.get("PI_TTS_MODEL", "mlx-community/Qwen3-TTS-12Hz-1.7B-CustomVoice-4bit")
VOICE = os.environ.get("PI_TTS_VOICE", "Aiden")
LANG = os.environ.get("PI_TTS_LANG", "English")
INSTRUCT = os.environ.get("PI_TTS_INSTRUCT", "")
INTERVAL = float(os.environ.get("PI_TTS_STREAM_INTERVAL", "0.32"))
URL = os.environ.get("PI_TTS_URL", "").rstrip("/")

emit({"type": "status", "message": "Loading voice"})

import numpy as np  # noqa: E402
import sounddevice as sd  # noqa: E402


class MlxVoice:
    """Qwen3-TTS on this Mac, through mlx-audio, streamed as it is made."""

    def __init__(self):
        import mlx.core as mx
        from mlx_audio.tts.utils import load_model

        self.mx = mx
        # Without a cap MLX's cache grows until the machine swaps.
        mx.set_cache_limit(int(0.5e9))
        self.model = load_model(MODEL)
        self.name = MODEL
        self.sample_rate = self.model.sample_rate

    def voices(self):
        return [str(n).lower() for n in self.model.get_supported_speakers()]

    def generate(self, text, voice, instruct):
        kw = dict(voice=voice, lang_code=LANG)
        if instruct:
            kw["instruct"] = instruct
        for r in self.model.generate(text=text, stream=True, streaming_interval=INTERVAL, **kw):
            self.mx.eval(r.audio)
            yield np.asarray(r.audio, dtype=np.float32).reshape(-1)


class ServerVoice:
    """Qwen3-TTS on a server (vllm-omni's /v1/audio/speech, as rpg_vox
    uses it): a sentence at a time, as WAV."""

    sample_rate = 24_000

    def __init__(self, url):
        self.url = url
        self.name = url

    def voices(self):
        return list(NATIVE)

    def generate(self, text, voice, instruct):
        import io
        import urllib.error
        import urllib.request

        import soundfile as sf

        body = {"input": text, "voice": voice, "language": LANG, "response_format": "wav"}
        if instruct:
            body["instructions"] = instruct
        req = urllib.request.Request(
            f"{self.url}/v1/audio/speech",
            data=json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                wav = resp.read()
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"{self.url} answered {e.code}: {e.read()[:300]!r}") from None
        audio, rate = sf.read(io.BytesIO(wav), dtype="float32", always_2d=True)
        audio = audio.mean(axis=1)
        if rate != self.sample_rate:
            n = round(len(audio) * self.sample_rate / rate)
            audio = np.interp(np.linspace(0, len(audio) - 1, n), np.arange(len(audio)), audio).astype(np.float32)
        # Handed over a third of a second at a time, so progress is smooth.
        step = self.sample_rate // 3
        for i in range(0, len(audio), step):
            yield audio[i : i + step]


class Player:
    """One output stream kept open; it plays queued audio, else silence.
    Each chunk belongs to a sentence, whose played samples are counted."""

    def __init__(self, rate):
        self.rate = rate
        self.chunks = deque()  # (sentence id, samples)
        self.lock = threading.Lock()
        self.current = None  # the sentence playing now
        self.played = {}  # sentence id -> samples played
        self.stream = sd.OutputStream(
            samplerate=rate, channels=1, dtype="float32", blocksize=1024, callback=self.fill
        )
        self.stream.start()

    def fill(self, out, frames, _time, _status):
        out.fill(0)
        filled = 0
        with self.lock:
            while filled < frames and self.chunks:
                sid, buf = self.chunks[0]
                n = min(frames - filled, len(buf))
                out[filled : filled + n, 0] = buf[:n]
                filled += n
                self.current = sid
                self.played[sid] = self.played.get(sid, 0) + n
                if n == len(buf):
                    self.chunks.popleft()
                else:
                    self.chunks[0] = (sid, buf[n:])

    def add(self, sid, samples):
        with self.lock:
            self.chunks.append((sid, samples))

    def clear(self):
        with self.lock:
            self.chunks.clear()
            self.current = None

    def queued(self, sid):
        """Samples of `sid` still waiting to play."""
        with self.lock:
            return sum(len(b) for s, b in self.chunks if s == sid)

    def empty(self):
        with self.lock:
            return not self.chunks


# The native language of each Qwen3-TTS CustomVoice speaker; any of them
# can speak English, the others with an accent.
NATIVE = {
    "aiden": "English",
    "ryan": "English",
    "vivian": "Chinese",
    "serena": "Chinese",
    "uncle_fu": "Chinese",
    "dylan": "Chinese, Beijing dialect",
    "eric": "Chinese, Sichuan dialect",
    "ono_anna": "Japanese",
    "sohee": "Korean",
}


class Speaker:
    def __init__(self, model):
        self.model = model
        self.voice = VOICE.lower()
        self.instruct = INSTRUCT
        self.player = Player(model.sample_rate)
        self.texts = queue.Queue()
        # Bumped by stop: sentences (and the one being generated) from an
        # older epoch are dropped.
        self.epoch = 0
        self.busy = False  # generating a sentence
        self.speaking = False
        self.last_id = None
        self.generated = {}  # sentence id -> samples generated
        self.complete = set()  # sentences fully generated
        threading.Thread(target=self.work, name="tts", daemon=True).start()
        threading.Thread(target=self.watch, name="tts-idle", daemon=True).start()

    def say(self, text, sid):
        self.texts.put((self.epoch, text, sid))

    def stop(self):
        self.epoch += 1
        self.complete.clear()
        self.generated.clear()
        self.player.played.clear()
        while True:
            try:
                _, _, sid = self.texts.get_nowait()
                self.last_id = sid if sid is not None else self.last_id
            except queue.Empty:
                break
        self.player.clear()

    def work(self):
        while True:
            epoch, text, sid = self.texts.get()
            if epoch != self.epoch:
                self.last_id = sid
                continue
            self.busy = True
            try:
                for audio in self.model.generate(text, self.voice, self.instruct):
                    if epoch != self.epoch:
                        break
                    self.player.add(sid, audio)
                    self.generated[sid] = self.generated.get(sid, 0) + len(audio)
                    if not self.speaking:
                        self.speaking = True
                        emit({"type": "speaking"})
            except Exception as e:  # keep serving the next sentence
                emit({"type": "error", "message": f"speaking failed: {e}", "fatal": False})
            finally:
                if epoch == self.epoch:
                    self.complete.add(sid)
                self.last_id = sid
                self.busy = False

    def progress(self, tick):
        """Report the sentence playing, and the ones that finished."""
        p = self.player
        rate = p.rate
        for sid in list(self.complete):
            if p.queued(sid) == 0 and p.played.get(sid, 0) >= self.generated.get(sid, 0):
                self.complete.discard(sid)
                p.played.pop(sid, None)
                self.generated.pop(sid, None)
                emit({"type": "said", "id": sid})
        sid = p.current
        if tick and sid is not None and sid in p.played:
            emit({
                "type": "progress",
                "id": sid,
                "played_s": round(p.played[sid] / rate, 3),
                "total_s": round(self.generated.get(sid, 0) / rate, 3) if sid in self.complete else None,
            })

    def watch(self):
        """Report progress; say `idle` once nothing is queued, generating or playing."""
        quiet_since = None
        sent_for = object()
        n = 0
        while True:
            time.sleep(0.05)
            n += 1
            self.progress(n % 2 == 0)
            idle = self.texts.empty() and not self.busy and self.player.empty()
            if not idle:
                quiet_since = None
                continue
            # The device still holds the last block or two.
            quiet_since = quiet_since or time.monotonic()
            if time.monotonic() - quiet_since < 0.15:
                continue
            if self.speaking or sent_for != self.last_id:
                self.speaking = False
                sent_for = self.last_id
                emit({"type": "idle", "last_id": self.last_id})


def main():
    t0 = time.perf_counter()
    model = ServerVoice(URL) if URL else MlxVoice()
    # The first generation compiles Metal kernels (or, with a server,
    # checks it answers); do it before anyone waits.
    for _ in model.generate("Ready.", VOICE.lower(), ""):
        pass
    print(f"loaded {model.name} in {time.perf_counter() - t0:.1f}s", file=sys.stderr)
    speaker = Speaker(model)
    emit({"type": "ready", "model": model.name, "voice": VOICE, "sample_rate_hz": model.sample_rate})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            cmd = json.loads(line)
        except json.JSONDecodeError as e:
            emit({"type": "error", "message": f"not JSON: {e}", "fatal": False})
            continue
        kind = cmd.get("type")
        if kind == "say":
            text = (cmd.get("text") or "").strip()
            if text:
                speaker.say(text, cmd.get("id"))
        elif kind == "stop":
            speaker.stop()
        elif kind == "voices":
            names = model.voices()
            emit({
                "type": "voices",
                "voices": [{"name": n, "native": NATIVE.get(n)} for n in names],
                "current": speaker.voice,
                "instruct": speaker.instruct,
            })
        elif kind == "set_voice":
            want = str(cmd.get("voice") or "").strip().lower().replace(" ", "_")
            names = model.voices()
            if want in names:
                speaker.voice = want
                if "instruct" in cmd:
                    speaker.instruct = str(cmd.get("instruct") or "").strip()
                emit({"type": "voice", "voice": want, "instruct": speaker.instruct})
            else:
                emit({"type": "error", "message": f"no voice {want!r}; there are {', '.join(names)}", "fatal": False})
        elif kind == "quit":
            break
        else:
            emit({"type": "error", "message": f"unknown command {kind!r}", "fatal": False})
    speaker.stop()


if __name__ == "__main__":
    try:
        main()
    except Exception as e:
        emit({"type": "error", "message": str(e), "fatal": True})
        sys.exit(1)

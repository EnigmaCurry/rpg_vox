# radio_drama

status: **v1 — speech only**

`radio_drama` turns a written radio play into a finished audio drama. You
give it the script and a recording of each voice; it gives you the whole
play as one file, every line spoken in its character's voice, paced like
a performance.

```sh
just install-radio-drama

radio_drama cast  play.md     # list the parts; fill in the voices
radio_drama check play.md     # what a render would do, without doing it
radio_drama render play.md    # the play
```

The voices are cloned with [Qwen3-TTS](qwen3-tts/) from three seconds of
reference audio each — the same clone deploy rpg_vox uses, so a checkout
already set up for that needs nothing added.

Music and sound effects are **not** produced. Every `[SFX]`, `[MUSIC]`
and `[TRANSITION]` cue in the script is kept, given a hole of silence to
sit in, and written to the manifest with the timestamps of that hole, so
a later pass has an exact place to put one.

## The script

The input is a radio play written as markdown — the document somebody
would have written anyway, not a format invented for this:

```markdown
## SCENE TWO — THE VIRUS WITHOUT A MACHINE

**INT. PRIVATE GROUND TRANSPORT — MOVING**

[SFX: The slow, uncomfortable rattle of an Imperial civilian
conveyance.]

**MAX:** Message from Voss.

**DRAKEN:** What's he found?

**VOSS** *(over vox)*: The message does not identify a location. I am
continuing analysis.

I shall remain on warm standby.

[PAUSE.]
```

What each part of that means:

| In the script | What it becomes |
|---|---|
| `**NAME:** text` | A line, spoken in that character's voice. |
| `**NAME** *(direction)*: text` | The same, with a performance direction. |
| A paragraph under a speaker, with no label of its own | The next beat of that speech, set closer than a new one. |
| `## HEADING` | A scene: a two-second break. |
| `**INT. SOMEWHERE — NIGHT**` | A slug line. Not spoken — a listener is told where they are by the sound. Kept as the best label there is for the ambience the scene wants. |
| `[PAUSE.]` / `[LONG PAUSE.]` | A beat with nothing in it. |
| `[SFX: …]`, `[MUSIC: …]`, `[TRANSITION: …]` | A hole of silence and a manifest entry. |

Four things about the format are worth knowing, because they are what
the parser is built around:

* **Paragraphs, not lines.** Scripts are hard-wrapped, so a cue or a
  speech routinely spans three source lines. Each blank-line-separated
  paragraph is unwrapped back into one before anything looks at it.

* **A speech continues until something else starts.** The label is
  written once; the paragraphs under it belong to that speaker until a
  cue, a heading or another label intervenes. Those paragraphs are
  deliberate beats — "The thirst." / "The screams." — so each becomes
  its own utterance with a shorter gap in front of it than a new speech
  gets.

* **A direction belongs to the speech.** `*(over vox)*` on the label
  carries to every paragraph under it, because that is what it means.

* **The script is longer than the play.** A title, a dramatis personae
  table and a convention note come first; production notes come after —
  and those notes contain `**Voss:**` paragraphs that are
  indistinguishable from dialogue by shape alone. So the play is bounded
  structurally: nothing counts until the first line or cue appears, and
  the first `#` heading *after* that ends it. In practice `# EPISODE III
  …` is ignored because nothing has been emitted yet, and `# PRODUCTION
  NOTES` ends the play. `--end-at TEXT` overrides this where the rule
  guesses wrong.

Speaker names are matched loosely, so `**Voss:**` and `**VOSS:**` are
one character, and `GM — TABLE`, `GM-TABLE` and `gm - table` are too:
case, every kind of dash and the spacing around `/` and `-` are not
facts about who is speaking.

## The cast file

`radio_drama cast play.md` reads the script and writes
`play.cast.toml`: every speaking part in it, in the order each is first
heard, with its line and word counts and nowhere to guess who should
sound like what.

```toml
[production]
language = "English"

# GM / NARRATOR — 318 lines, 2247 words
[cast."GM / NARRATOR"]
sample = "voices/narrator.wav"
ref_text = "There are few things in the Imperium more dangerous than an unanswered question."

# GM — TABLE — 35 lines, 241 words
[cast."GM — TABLE"]
same_as = "GM / NARRATOR"

# FE — 28 lines, 144 words
[cast."FE"]
server_voice = "old-man"
gain_db = -1.5
```

A part is cast one of three ways:

| Key | Means |
|---|---|
| `sample` | Reference audio on disk — wav, flac, mp3 or ogg/vorbis, and anything else `ffmpeg` can read — registered with the clone server for you. Three seconds is enough. |
| `ref_text` | What the sample says. Optional; the server builds the voice either way, but giving it makes the clone closer. |
| `server_voice` | A voice already registered on the server. `radio_drama voices` lists them. |
| `same_as` | Another part in this file, whose voice this one shares. |

and may also set `language` and `gain_db`.

The game master narrating and the game master adjudicating at the table
are one person, so a `GM`-prefixed part that is not the narrator is
scaffolded as `same_as` the narrator rather than left to fill in.
Change it if you want them to sound different.

Re-running `cast` after the script gains a character keeps everything
already assigned and adds the new parts; `--force` starts over.

Sample-based voices are **content-addressed**. The same wav always
registers under the same server-side name, so a second render uploads
nothing — and a *changed* wav gets a different name, which also
invalidates every cached line that voice had spoken. Nothing has to be
cleared by hand.

### Pacing

Every gap in the play is a number in `[production]`, in seconds:

| Option | Default | Between |
|---|---|---|
| `paragraph_s` | 0.4 | Paragraphs of one speech. |
| `line_s` | 0.5 | Two speeches by the same character. |
| `speaker_s` | 0.65 | Two characters. |
| `pause_s` | 1.0 | `[PAUSE.]` |
| `long_pause_s` | 2.5 | `[LONG PAUSE.]` |
| `cue_s` | 0.8 | The hole left for an `[SFX]` / `[MUSIC]` / `[TRANSITION]`. |
| `scene_s` | 2.0 | A `##` scene break. |
| `slug_s` | 0.0 | A slug line. |

and three that decide how tightly the synthesized lines butt up against
each other: `trim_db` (−45), the level below which the padding either
side of an utterance counts as silence and is cut; `trim_keep_ms` (40),
how much of that silence to keep so a cut never lands on the first
consonant; and `fade_ms` (5), the ramp at each edge, because
concatenating at a non-zero sample clicks.

The server's padding is not consistent, so without the trim the pacing
above is advisory at best.

### Performance directions

The clone model takes no style input. It accepts an `instructions`
field and ignores it — the same text with and without a style
instruction comes back byte-identical often enough to settle the
question — so a direction can only be honoured by processing the audio
that comes back.

Level is honest. A band-pass with a little saturation is a fair reading
of "over vox". Anything past that would be pretending, so these are the
defaults and the rest are reported rather than faked:

| Direction contains | What happens |
|---|---|
| `barely audible` | −14 dB |
| `very quietly` | −10 dB |
| `whisper` | −9 dB |
| `under his breath` / `under her breath` | −8 dB |
| `quietly` / `softly` | −6 dB |
| `loud` | +3 dB |
| `over vox` | 300–3400 Hz band-pass, saturated, −1.5 dB |
| `internal voice` | 6 kHz low-pass, −3 dB |

Keys are matched as substrings, longest first, so `(very quietly)` is
not read as `(quietly)` and `(quietly, to Max)` still is. A
`[directions."over vox"]` table in the cast file replaces the entry of
that name entirely; it takes `gain_db`, `highpass_hz`, `lowpass_hz` and
`drive` (0.0–1.0).

Directions nothing matches — `(with ritual gravity)`, `(laughing)` —
are notes to a human performer. They are stripped from what is spoken,
so the synthesizer never reads one aloud, and `check` lists them.

## Rendering

```sh
radio_drama render play.md
```

writes, into a directory named after the script and beside it:

```
session_3_radio_play_revised/
  session_3_radio_play_revised.flac           the play
  session_3_radio_play_revised.manifest.json  what happens when
  cache/<sha256>.wav                          one per distinct line
```

Three passes, each able to fail before the next has cost anything:

1. **Plan.** Work out every utterance that has to exist, every gap, and
   where each lands. Nothing is synthesized and nothing is read — this
   is what `check` prints, and what reports the play's length before a
   single request goes out.
2. **Synthesize.** Fill the cache, `--jobs` requests at a time.
   Identical text in the same voice is one utterance however many times
   the script says it, and anything already cached is skipped.
3. **Mix.** Read the cache back, trim, apply the cast's levels and the
   directions' effects, and lay it out against the plan's gaps.

Every effect lives in the third pass, so the cache holds exactly what
the server sent. Re-pacing or re-balancing a whole play is then a
seconds-long job rather than a re-render, and editing one line costs one
request. `--offline` mixes from the cache alone and refuses to render if
anything is missing.

| Option | Default | |
|---|---|---|
| `--out DIR` | `<script>/` beside the script | Where the mixdown, manifest and cache go. |
| `--format flac\|wav` | `flac` | FLAC needs `ffmpeg` on PATH. |
| `--jobs N` | 8 | Synthesis requests in flight at once. |
| `--refresh` | off | Re-synthesize every line even if cached. |
| `--offline` | off | Mix from the cache; make no requests. |
| `--peak-db DB` | −1.0 | Level the finished mix is pulled down to if it exceeds it. A mix under it is left alone. |
| `--cast FILE` | `<script>.cast.toml` | |
| `--end-at TEXT` | | Stop reading the play at the first heading containing this. |
| `--url URL` | `$RADIO_DRAMA_CLONE_URL`, else `$RPG_VOX_QWEN3_CLONE_URL` | The Qwen3-TTS clone deploy. |
| `--timeout SECS` | 180 | Per request. The first one after the container starts waits on CUDA graph capture. |

The mixdown is mono at the synthesizer's own 24 kHz. That is the one
rate in the pipeline: a play is speech, and resampling it to something
rounder would cost quality and size to gain nothing.

`RUST_LOG=radio_drama=info` shows each request and registration.

## The manifest

The third output, and the reason the cues are kept. Every scene, slug,
line, pause and cue, in order, with the timestamps of the audio or
silence it occupies:

```json
{
  "kind": "cue",
  "start_s": 2.0,
  "end_s": 2.8,
  "scene": "PROLOGUE — TWO DESTINATIONS",
  "cue": "SFX",
  "text": "Ecclesiastical bells. Traffic. Distant crowds."
}
{
  "kind": "line",
  "start_s": 356.05,
  "end_s": 360.12,
  "scene": "SCENE TWO — THE VIRUS WITHOUT A MACHINE",
  "speaker": "VOSS",
  "voice": "rd-9eea1d392f266e3a",
  "direction": "over vox",
  "effect": "over vox",
  "text": "The message does not identify a location.",
  "cache": "57c05919…"
}
```

So scoring the production later is a matter of reading this file:

```sh
jq -r '.entries[] | select(.kind=="cue") | "\(.start_s)\t\(.cue)\t\(.text)"' \
  play/play.manifest.json
```

## What v1 does not do

* **No music or sound effects.** The cues are placed and timed, not
  produced.
* **Mono.** A stereo field is where effects and music want to live, so
  it belongs with them rather than ahead of them.
* **No delivery control.** See [Performance
  directions](#performance-directions): the model has no style input,
  and this does not pretend otherwise.

## Requirements

* A Qwen3-TTS clone deploy — the `clone` service in
  [qwen3-tts/](qwen3-tts/), serving `Qwen/Qwen3-TTS-12Hz-1.7B-Base`.
  Set `RADIO_DRAMA_CLONE_URL`, or `RPG_VOX_QWEN3_CLONE_URL` which `.env`
  already carries.

* `ffmpeg` on PATH, for two things: writing FLAC, which is the default
  output format, and reading any reference recording symphonia cannot —
  Ogg Opus in particular, which is what scribe records, since the only
  Opus decoder that works is a C library. `--format wav` plus references
  in wav, flac, mp3 or ogg/vorbis needs no ffmpeg at all.

  FLAC goes through ffmpeg rather than an in-process encoder because
  flacenc 0.5.1 writes frame numbers that `flac -t` reports as "file
  might not be seekable" and that symphonia refuses to open at all.

* **No system libraries.** Unlike the rest of this workspace,
  `radio_drama` links nothing but libc: it reads files and writes files,
  and never opens an audio device, so it does not go near PipeWire,
  ALSA or libopus. `cargo build -p radio_drama` works outside the nix
  shell.

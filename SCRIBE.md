# scribe

https://github.com/user-attachments/assets/4fc6dbd3-05c7-4c80-b4ab-5aaae2d44119
*Transcription of Abbott and Costello's Who's on First?*

Live transcription in your terminal, using speech models that run on your
own machine. Words appear as they are spoken, then get quietly corrected
as better passes catch up, and the result is saved as clean markdown, with
subtitles and audio if you want them.

* **Instant, then accurate.** A streaming model shows words right away; a
  stronger recognizer re-decodes each utterance when you pause, repairs
  words cut at the edges, and (optionally) an LLM proofreads the paragraph.
* **Local and private.** Once the models are downloaded, recognition
  runs offline. Nothing leaves your machine unless you turn on LLM
  proofreading, which can also point at a local server.
* **Any source.** Your mic, the audio of one app (a call, a video, a
  stream), or a wav/flac/mp3/ogg file.
  * **Real files.** Markdown you can `tail -f`, `.srt` and karaoke `.ass`
  subtitles, an `.opus` recording, and playback in the terminal with the
  current word lit up.
* **Who said what.** Speaker labels by diarization, or one input per
  speaker so crosstalk is transcribed in full.
* **Your names, spelled right.** A vocabulary file teaches it names and
  terms it couldn't know.
* **One-key dictation.** Press a key, talk, press enter, and the text is
  on your clipboard.

macOS (Apple Silicon) and Linux, prebuilt binaries on the
[releases page](https://github.com/EnigmaCurry/rpg_vox/releases).
`scribe` is the standalone companion to [rpg_vox](RPG_VOX.md) and shares
its transcription passes, with nothing RPG-specific in it.

* [Quick start](#quick-start)
* [How it works](#how-it-works)
* [Install](#install)
* [Audio sources](#audio-sources)
* [Output files](#output-files)
* [Pass 4: LLM proofreading](#pass-4-llm-proofreading-optional)
* [Vocabulary](#vocabulary-optional)
* [Speakers](#speakers-optional)
* [Models and logs](#models-and-logs)
* [One-shot dictation key](#one-shot-dictation-key)
* [Reference: every option and key](SCRIBE_REFERENCE.md)

## Quick start

After [installing](#install):

```bash
scribe download-models        # fetch the models now instead of on first run
scribe devices                # list inputs
scribe apps                   # list apps whose audio can be captured
scribe                        # transcribe the default mic; nothing is saved
scribe -o notes               # ... and append each paragraph to notes.md
scribe -r session             # record session.md/.srt/.ass/.opus
scribe -p session             # play the recording back with karaoke subtitles
scribe -i talk.mp3 -o talk    # transcribe a file without the TUI
scribe --once                 # dictate one paragraph to the clipboard
```

Every option is listed in `scribe --help` and in the
[reference](SCRIBE_REFERENCE.md#command-line-reference); the keys for the
TUI are [there too](SCRIBE_REFERENCE.md#keys).

## How it works

Each utterance goes through up to four passes:

1. **Streaming** (Zipformer): words appear instantly, in dim ALL CAPS.
2. **Re-decode** (Parakeet, or SenseVoice with `--model sensevoice`):
   each utterance is re-transcribed when you pause.
3. **Boundary fix** (same recognizer): the last 2–3 utterances are
   re-decoded together to repair words cut at utterance edges. Changed
   words flash green.
4. **LLM proofreading** (optional, `--llm`): each settled paragraph gets
   small edits from an OpenAI-compatible chat model. Changed words are
   blue.

Audio comes from CoreAudio on macOS and PipeWire on Linux (the
`vox_audio` crate). The first run downloads the models (about 580 MB) if
none are found; `scribe download-models` does the same thing up front.

## Install

Prebuilt binaries are attached to each
[GitHub release](https://github.com/EnigmaCurry/rpg_vox/releases) for
macOS (Apple Silicon) and Linux (x86_64 and aarch64). Pick the archive for
your platform, unpack it, put `scribe` on your `PATH` and fetch the models:

```bash
VERSION=v0.1.0
TARGET=aarch64-apple-darwin   # or x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu
curl -fLO "https://github.com/EnigmaCurry/rpg_vox/releases/download/$VERSION/scribe-$VERSION-$TARGET.tar.gz"
tar -xzf "scribe-$VERSION-$TARGET.tar.gz"
install -m 755 "scribe-$VERSION-$TARGET/scribe" ~/.local/bin/scribe
scribe download-models
```

<details>
<summary>Checksums, the macOS quarantine flag, Linux requirements</summary>

Each archive has a `.sha256` beside it to check the download against
(`shasum -a 256 -c scribe-$VERSION-$TARGET.tar.gz.sha256`). The macOS
binary is not signed: if you downloaded it with a browser instead of
`curl`, clear the quarantine flag with `xattr -d com.apple.quarantine
~/.local/bin/scribe`. The Linux binaries need PipeWire (`libpipewire-0.3`)
and glibc 2.39 or newer (Ubuntu 24.04, Fedora 40, or later).

</details>

The release archive has only the binary. The macOS
[dictation key](#one-shot-dictation-key) launchers and Karabiner rule come
from a source install.

### From source

```bash
just install-scribe              # or: just install-scribe /usr/local/bin
```

This builds the release binary, copies it to `~/.local/bin/scribe`, and
downloads the models into the per-user data dir (skipped if already
there). Make sure the install directory is on your `PATH`. On macOS it
also installs `scribe-once.terminal` next to the binary and drops the
Karabiner rule for the [dictation key](#one-shot-dictation-key) into
`~/.config/karabiner/assets/complex_modifications/`.

On macOS, build with plain cargo (no nix-shell), and use `-p vox_scribe`:
the rpg_vox and discord_vox crates need PipeWire and only build on Linux.
sherpa-onnx is linked statically, so the binary is self-contained on both
macOS and Linux:

```bash
cargo build -p vox_scribe --release        # target/release/scribe
cargo install --path crates/vox_scribe     # or install into ~/.cargo/bin
```

From a checkout, `just scribe ARGS` runs it (fetching the models first
and loading `.env`).

## Audio sources

* **Default**: the system input.
* `--device NAME`: an input by id or name substring (see `devices`). On
  Linux, a sink's node name captures what is playing on it.
* `--app NAME`: transcribe what one app is playing (see `apps`). Every
  matching process is captured, so `--app chrome` also gets Chrome's
  helper processes, and new ones are picked up while running.
  * macOS 14.4+: a Core Audio process tap. The first run asks for
    *System Audio Recording* permission for your terminal app (System
    Settings → Privacy & Security → Screen & System Audio Recording);
    without it the tap records silence rather than failing. Each
    terminal needs it separately: the F5 [dictation key](SCRIBE_REFERENCE.md#macos--f5-key)
    runs scribe in Ghostty (or Terminal), so allow that one too to pick
    apps there. An app only shows up once it has opened an audio output.
  * Linux: scribe's PipeWire stream is linked to the app's output streams
    with `pw-link`.
* `--virtual-sink` (Linux): a `vox_scribe` PipeWire sink; route apps into
  it. On macOS use `--app`, or a loopback device such as BlackHole with
  `--device`.
* `--input FILE`: a wav/flac/mp3/ogg file, transcribed as fast as
  possible without the TUI, or in real time with `--live`.

### One source per speaker

When each person has their own channel, transcribe the channels
separately instead of diarizing a mix: repeat `-d` and/or `-a` (or `-i`
for per-speaker tracks of one session, all starting together), and each
source becomes one speaker, A, B, … in command-line order:

```bash
scribe -d "USB Mic" -a Discord --speakers "Me,Party" -r session
scribe -i alice.flac -i bob.flac --speakers Alice,Bob -o game
```

Every source runs its own pipeline, so people talking over each other
are each transcribed in full. When someone starts talking in the middle
of another speaker's paragraph, that paragraph is split at the sentence
end nearest the interruption, so the two read as a back-and-forth (live
and in the files). `--speakers` takes their names in the same order, and
`n` renames them as with `--diarize` (which this replaces: the two don't
combine). The level meter shows the loudest source, `--record` mixes
them all into one `.opus`, and at the end the files are rewritten in time
order, since sources settle out of step.

## Output files

With `-o NAME`, each paragraph is appended to `NAME.md` once it settles
(2 s of quiet; in manual mode, 2 s of quiet after `enter`), so
`tail -f NAME.md` follows along. Quitting appends whatever is still open.
Paragraphs are wrapped to 79 columns.

With `-r NAME`, you also get:

* `NAME.srt`: subtitles.
* `NAME.ass`: karaoke subtitles that light up each word as it is spoken.
  They work in other players too, e.g. `mpv --sub-file=NAME.ass video.mp4`.
* `NAME.opus`: the audio exactly as the transcriber heard it, mono Opus
  at 24 kb/s, about 10 MB an hour.

Word times come from the offline recognizer's token timestamps and are
carried through the boundary and LLM passes. `scribe -p NAME` plays the
recording back in the terminal with the current word lit up.

Copying (`y`, `--once`) uses `pbcopy`, `wl-copy` or `xclip`, falling back
to the OSC 52 terminal escape (e.g. over ssh).

## Pass 4: LLM proofreading (optional)

Pass `--llm` and each paragraph is proofread by an OpenAI-compatible chat
endpoint once it settles, before it is written to the file. The model
returns small edits (number and time formats, misheard names, sentence
breaks inserted mid-sentence) rather than rewriting, and edits that would
change more than 40% of a paragraph's words are rejected.

The endpoint is configured with environment variables, which `just
scribe` also loads from the repo's `.env` file:

```bash
VOX_SCRIBE_LLM_URL=https://api.openai.com/v1   # default
VOX_SCRIBE_LLM_MODEL=<model>                   # required
OPENAI_API_KEY=...                             # or VOX_SCRIBE_LLM_KEY
```

```bash
just scribe --llm --vocab names.txt -o notes
```

Local servers (llama.cpp, ollama, vLLM) work the same way: set
`VOX_SCRIBE_LLM_URL` to their `/v1` URL; the key is optional. After a
failure or a timeout (`--llm-timeout`) the paragraph is written with its
pass-3 text and its timestamp turns red.

In the TUI, words pass 4 changed are blue, a paragraph's timestamp is
bold blue while it waits for the LLM, and `o` toggles the text from
before pass 4. Without `--llm`, pass 3 is the final pass and paragraphs
are written as soon as they settle.

## Vocabulary (optional)

`--vocab FILE` lists names and terms the recognizer can't know (one per
line, `#` for comments), e.g. the places and characters of a campaign.
With Parakeet, decoding is biased toward them (sherpa-onnx hotwords with
beam search). Biasing on its own invents names in ordinary speech, so a
term is only kept where an unbiased decode of the same audio heard
something that sounds like it ("Icemark" → "Ismark", "Valaki" →
"Vallaki"); everywhere else the unbiased words stand. The unbiased decode
only runs when the biased one produced a term. Cost: passes 2 and 3 run
about 20% slower, up to ~1.8x on name-dense speech; the live text and
diarization are unaffected, and without `--vocab` nothing changes. With
`--llm`, pass 4 gets the list too.

On synthetic test dialogue it got 13 of 20 made-up names right (3
without it) and invented none in sound-alike sentences, with one
exception: a homophone spelling of a listed name ("Tatiana" for listed
"Tatyana") becomes the listed one, since only context could tell them
apart. Names it still misses come out as before (e.g. "Irena" for
"Ireena").

## Speakers (optional)

Pass `--diarize` to label who is speaking. Two passes:

* **Live**: as each utterance is transcribed, a voice embedding
  (3D-Speaker ERes2NetV2) matches it to the speakers heard so far or
  starts a new one. Paragraphs start on a change of speaker and show as
  `Speaker A: …`, one colour per speaker. It is quick and approximate.
* **Final**: with `-r`, or `-i FILE -o NAME`, a full offline diarization
  (pyannote segmentation + embeddings + clustering) of the whole audio
  runs after the session ends, with a progress bar on the console
  (`ctrl+c` skips it). It assigns every word to a speaker, splits
  paragraphs where the speaker changes, and atomically rewrites `NAME.md`,
  `.srt` and `.ass`. The files are written as usual during the session,
  with the live labels, so a crash or a skip still leaves them complete.
  `--append` keeps the live labels only.

The clustering always picks the number of voices itself; clusters with
little speech (laughter, a jingle: under 10 s, or 5% of a short
recording) are folded into the closest real speaker, and `--speakers`
keeps the biggest N. Each sentence is then matched to a speaker by its
own voice.

Speakers are lettered in order of first appearance (of a run of a few
words, so a jingle at the start doesn't take the first letter). The
markdown reads `**[00:01:02] Speaker A:** …`, SRT cues start
`Speaker A: ` where the speaker changes, and ASS lines carry the speaker
in their Name field with a style (colour) per speaker, which `--play`
shows.

`--speakers` fixes the count exactly: every voice is put with one of
them, so leave it out for open-ended sessions where people may come and
go. Names given there, or typed in the `n` speaker list, are used
everywhere "Speaker A" would be. Names follow letters, and the final pass
letters speakers by first appearance like the live pass, so a name can
land on a different voice when the live labels were wrong.

The speaker models (~77 MB) are fetched on first use, or with
`scribe --diarize download-models` / `just download-speaker-models`.

## Models and logs

Models are looked up in `--models-dir`, `$VOX_SCRIBE_MODELS`, the
per-user data dir (`~/Library/Application Support/vox_scribe/models` on
macOS, `${XDG_DATA_HOME:-~/.local/share}/vox_scribe/models` on Linux),
then `./models` (so an rpg_vox checkout's models work as-is). In TUI
mode, logs go to `$TMPDIR/vox_scribe.log` unless `--log` says otherwise.

## One-shot dictation key

`scribe --once` works as a push-to-talk dictation tool: bind a key that
opens a terminal running it, speak, press `enter`, and the text is on the
clipboard when the window closes. A second press of the same key can run
`scribe stop-once` to finish it.

Setup for the macOS 🎤 / F5 key (with Karabiner-Elements) and for GNOME,
KDE, Sway, i3 and Hyprland is in the
[reference](SCRIBE_REFERENCE.md#one-shot-dictation-key).

## Reference

[SCRIBE_REFERENCE.md](SCRIBE_REFERENCE.md) lists every command, option,
environment variable and key.

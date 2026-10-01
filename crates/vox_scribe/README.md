# vox_scribe

Live transcription in the terminal, saved as markdown. It uses the same
layered passes as rpg_vox's Record page, via the `vox_transcribe` crate:

1. **Streaming** (Zipformer): words appear instantly, in dim ALL CAPS.
2. **Re-decode** (SenseVoice): each utterance is re-transcribed when you pause.
3. **Boundary fix** (SenseVoice): the last 2–3 utterances are re-decoded
   together to repair words cut at utterance edges. Changed words flash green.

Audio comes from CoreAudio on macOS and PipeWire on Linux (`vox_audio`).

```bash
cargo run -p vox_scribe --release -- download-models
cargo run -p vox_scribe --release -- devices
cargo run -p vox_scribe --release -- -o notes.md
```

Keys: `space` pause, `enter` new paragraph, `s` save now, `↑↓ PgUp PgDn`
scroll, `End` follow, `q` save and quit. The markdown file is rewritten each
time a paragraph settles (2 s of quiet), and in full on exit.

Other options:

* `--device NAME`: an input by id or name substring (see `devices`). On
  Linux, a sink's node name captures what is playing on it.
* `--virtual-sink` (Linux): register a `vox_scribe` PipeWire sink and
  transcribe whatever apps play into it. On macOS use a loopback device
  such as BlackHole with `--device`.
* `--input FILE`: transcribe a wav/flac/mp3/ogg file.
* `--no-tui`: print paragraphs to stdout as they settle.
* `--language en`: pin SenseVoice's language (default `auto`).

Models are looked up in `--models-dir`, `$VOX_SCRIBE_MODELS`, the per-user
data dir, then `./models` (so an rpg_vox checkout's models work as-is). In
TUI mode, logs go to `$TMPDIR/vox_scribe.log`.

On macOS, build with plain cargo (no nix-shell), and use `-p vox_scribe`:
the rpg_vox and discord_vox crates need PipeWire and only build on Linux.

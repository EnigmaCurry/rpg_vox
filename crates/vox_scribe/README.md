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

Keys: `↑↓` select a paragraph, `y` copy it (the newest one if none is
selected), `PgUp PgDn` scroll, `End` follow, `space` pause, `enter` new
paragraph, `m` switch auto/manual paragraphs, `q` quit. Nothing is saved
to disk unless you pass `-o FILE`. With it, each paragraph is appended to
the markdown file once it settles (2 s of quiet; in manual mode,
2 s of quiet after `enter`), so `tail -f notes.md` follows along; quitting
appends whatever is still open. Copying uses `pbcopy`, `wl-copy` or `xclip`,
falling back to the OSC 52 terminal escape (e.g. over ssh).

Other options:

* `--device NAME`: an input by id or name substring (see `devices`). On
  Linux, a sink's node name captures what is playing on it.
* `--virtual-sink` (Linux): register a `vox_scribe` PipeWire sink and
  transcribe whatever apps play into it. On macOS use a loopback device
  such as BlackHole with `--device`.
* `--input FILE`: transcribe a wav/flac/mp3/ogg file.
* `--append`: add to an existing output file, after a `---` rule and a new
  date line. The TUI shows the file's previous content in grey above the
  new session. Without `--append`, an existing file is an error.
* `--manual`: start new paragraphs only when you press `enter`, not on
  silence or word count. Press `m` in the TUI to switch modes; the header
  shows `¶ AUTO` or `¶ MANUAL`.
* `--once`: one-shot recorder. Records a single manual paragraph until you
  press `enter`, then finishes every pass (including `--llm`), prints the
  text, copies it to the clipboard and exits. `q`, `esc` or `ctrl-c`
  cancels without copying.
* `--no-tui`: print paragraphs to stdout as they settle.
* `--language en`: pin SenseVoice's language (default `auto`).

## Pass 4: LLM proofreading (optional)

Pass `--llm` and each paragraph is proofread by an OpenAI-compatible chat
endpoint once it settles, before it is written to the file. The model returns
small edits (number and time formats, misheard names, sentence breaks inserted
mid-sentence) rather than rewriting, and edits that would change more than 40%
of a paragraph's words are rejected.

The endpoint is configured with environment variables, which `just scribe`
also loads from the repo's `.env` file:

```bash
VOX_SCRIBE_LLM_URL=https://api.openai.com/v1   # default
VOX_SCRIBE_LLM_MODEL=<model>                   # required
OPENAI_API_KEY=...                             # or VOX_SCRIBE_LLM_KEY
```

```bash
just scribe --llm --vocab names.txt -o notes.md
```

* `--vocab FILE`: names and terms, one per line, to help with spelling.
* `--llm-timeout SECS` (default 30): after a failure or timeout the paragraph
  is written with its pass-3 text and its timestamp turns red.
* Local servers (llama.cpp, ollama, vLLM) work the same way: set
  `VOX_SCRIBE_LLM_URL` to their `/v1` URL; the key is optional.

In the TUI, words pass 4 changed are blue, a paragraph's timestamp is bold
blue while it waits for the LLM, and `o` toggles the text from before pass 4.
Without `--llm`, pass 3 is the final pass and paragraphs are written as
soon as they settle.

 `--models-dir`, `$VOX_SCRIBE_MODELS`, the per-user
data dir, then `./models` (so an rpg_vox checkout's models work as-is). In
TUI mode, logs go to `$TMPDIR/vox_scribe.log`.

On macOS, build with plain cargo (no nix-shell), and use `-p vox_scribe`:
the rpg_vox and discord_vox crates need PipeWire and only build on Linux.
sherpa-onnx is linked statically, so the binary is self-contained on both
macOS and Linux:

```bash
cargo build -p vox_scribe --release   # target/release/vox_scribe
cargo install --path crates/vox_scribe   # or install into ~/.cargo/bin
```

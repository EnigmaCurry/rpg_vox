# vox_scribe

Live transcription in the terminal, saved as markdown. It uses the same
layered passes as rpg_vox's Record page, via the `vox_transcribe` crate:

1. **Streaming** (Zipformer): words appear instantly, in dim ALL CAPS.
2. **Re-decode** (Parakeet): each utterance is re-transcribed when you pause.
3. **Boundary fix** (Parakeet): the last 2–3 utterances are re-decoded
   together to repair words cut at utterance edges. Changed words flash green.

Audio comes from CoreAudio on macOS and PipeWire on Linux (`vox_audio`).
The first run downloads the models (about 580 MB) if none are found;
`download-models` does the same thing up front. The crate is `vox_scribe`;
the binary it builds is `scribe`.

```bash
cargo run -p vox_scribe --release -- download-models
cargo run -p vox_scribe --release -- devices
cargo run -p vox_scribe --release -- apps
cargo run -p vox_scribe --release -- -o notes
cargo run -p vox_scribe --release -- -r session
cargo run -p vox_scribe --release -- -p session
```

Keys: `↑↓` select a paragraph, `y` copy it (the newest one if none is
selected), `PgUp PgDn` scroll, `End` follow, `space` pause (releases the mic, so the
OS stops showing it in use; timestamps keep running), `enter` new
paragraph, `m` switch auto/manual paragraphs, `q` quit. Nothing is saved
to disk unless you pass `-o NAME`. With it, each paragraph is appended to
`NAME.md` once it settles (2 s of quiet; in manual mode,
2 s of quiet after `enter`), so `tail -f notes.md` follows along; quitting
appends whatever is still open. Copying uses `pbcopy`, `wl-copy` or `xclip`,
falling back to the OSC 52 terminal escape (e.g. over ssh).

`-r NAME` (`--record`) records a session instead: `NAME.md` as above,
plus `NAME.srt` (subtitles), `NAME.ass` (karaoke subtitles that light up
each word as it is spoken) and `NAME.opus` (the audio exactly as the
transcriber heard it, mono Opus at 24 kb/s, about 10 MB an hour). Word
times come from the offline recognizer's token timestamps and are
carried through the boundary and LLM passes. The subtitles also work in
other players, e.g. `mpv --sub-file=NAME.ass video.mp4`.

`-p NAME` (`--play`) plays a recording back in the terminal: the audio
with the subtitles printed as they are spoken, the current word lit up.
`space` pauses, `←` `→` jump to the previous / next word, `↑` `↓` to
the previous / next line (subtitle cue), `home` restarts, `q` quits. `/` searches like `less`: type a pattern
(case-insensitive, may span words) and `enter` to jump to the next
match, then `n` / `p` (or `N`) for the next / previous match, wrapping
at the ends; matches are underlined and `esc` clears them.

Other options:

* `--device NAME`: an input by id or name substring (see `devices`). On
  Linux, a sink's node name captures what is playing on it.
* `--app NAME`: transcribe what one app is playing, by PID or by a
  substring of its process name or bundle id (see `apps`, which marks the
  ones playing now with `*`). Every matching process is captured, so
  `--app chrome` also gets Chrome's helper processes, and new ones are
  picked up while running. The app keeps playing to your speakers.
  * macOS 14.4+: a Core Audio process tap. The first run asks for
    *System Audio Recording* permission for your terminal app (System
    Settings → Privacy & Security → Screen & System Audio Recording);
    without it the tap records silence rather than failing. An app only
    shows up once it has opened an audio output.
  * Linux: vox_scribe's PipeWire stream is linked to the app's output
    streams with `pw-link`.
* `--virtual-sink` (Linux): register a `vox_scribe` PipeWire sink and
  transcribe whatever apps play into it. On macOS use `--app`, or a
  loopback device such as BlackHole with `--device`.
* `--input FILE`: transcribe a wav/flac/mp3/ogg file. No TUI: brief
  progress goes to stderr, and the paragraphs to stdout unless `-o` / `-r`
  are writing them to files.
* `--live` (with `--input`): open the TUI instead and play the file to
  the speakers, transcribing it in real time as if it came from the mic.
  `space` pauses both; times stay in file time. No seeking (that is what
  `--play` is for).
* `--append`: add to an existing output file, after a `---` rule and a new
  date line. The TUI shows the file's previous content in grey above the
  new session. Without `--append`, an existing file is an error.
* `--manual`: start new paragraphs only when you press `enter`, not on
  silence or word count. Press `m` in the TUI to switch modes; the header
  shows `¶ AUTO` or `¶ MANUAL`.
* `--once`: one-shot recorder. Records a single manual paragraph until you
  press `enter`, then finishes every pass (including `--llm`), prints the
  text, copies it to the clipboard and exits. `q`, `esc` or `ctrl-c`
  cancels without copying. `shift+enter` starts a new paragraph without
  finishing (in normal mode it's the same as `enter`); it needs a terminal
  with the kitty keyboard protocol (Ghostty, kitty, WezTerm, iTerm2), since
  macOS Terminal sends the same code for both. Nothing is saved to disk:
  `-o`, `--append` and the `s` key are off in this mode. Only one runs at a time: launching a second
  `--once` (or running `scribe stop-once`) finishes and copies the
  running one instead, so one key can both start and stop dictation.
* `--no-tui`: print paragraphs to stdout as they settle.
* `--model sensevoice` (or `VOX_SCRIBE_MODEL=sensevoice`): use SenseVoice
  Small for passes 2 and 3 instead of the default NVIDIA Parakeet TDT 0.6B
  v2. Parakeet is English only and noticeably more accurate on English;
  SenseVoice is multilingual, about twice as fast, uses half the memory
  (~620 MB vs ~1.2 GB on an M1) and is a smaller download (155 MB vs
  460 MB, fetched on first use).
* `--language en`: pin SenseVoice's language (default `auto`; SenseVoice
  only).

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
just scribe --llm --vocab names.txt -o notes
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

## Speakers (optional)

Pass `--diarize` to label who is speaking. Two passes:

* **Live**: as each utterance is transcribed, a voice embedding
  (WeSpeaker ResNet34) matches it to the speakers heard so far or starts
  a new one. Paragraphs start on a change of speaker and show as
  `Speaker A: …`, one colour per speaker. It is quick and approximate.
* **Final**: with `-r`, or `-i FILE -o NAME`, a full offline diarization
  (pyannote segmentation + embeddings + clustering) of the whole audio
  runs after the session ends, however long it takes (`q` skips it in
  the TUI). It assigns every word to a speaker, splits paragraphs where
  the speaker changes, and atomically rewrites `NAME.md`, `.srt` and
  `.ass`. The files are written as usual during the session, with the
  live labels, so a crash or a skip still leaves them complete.

Speakers are lettered in order of first appearance. The markdown reads
`**[00:01:02] Speaker A:** …`, SRT cues start `Speaker A: ` where the
speaker changes, and ASS lines carry the speaker in their Name field
with a style (colour) per speaker, which `--play` shows. `--append` keeps
the live labels only.

* `--speakers N` or `--speakers "Alice,Bob,Carol"`: who is speaking,
  when known. A number fixes the count; names fix the count to how many
  there are and name the speakers in the order they are first heard
  (A, B, …), used everywhere "Speaker A" would be. The count is exact,
  not a hint: every voice is put with one of them, so leave it out for
  open-ended sessions where people may come and go.

In the TUI, `n` lists the speakers (those heard so far, or A… up to
the `--speakers` count): `↑`/`↓` select, `enter` types a new name, `del`
restores "Speaker A", `esc` closes. Renaming applies straight away on
screen; files already written get the new names when the session ends.
Names go into the ASS Name field, so `--play` shows them too. Names
follow letters, and the final pass letters speakers by first appearance
like the live pass, so a name can land on a different voice when the
live labels were wrong.
* `--speaker-threshold X` (default 0.7): how alike a voice must be to a
  known speaker to join them live. Raise it if two people share a label,
  lower it if one person is split in two.

The speaker models (~32 MB) are fetched on first use, or with
`scribe --diarize download-models` / `just download-speaker-models`.

Models are looked up in `--models-dir`, `$VOX_SCRIBE_MODELS`, the per-user
data dir (`~/Library/Application Support/vox_scribe/models` on macOS,
`${XDG_DATA_HOME:-~/.local/share}/vox_scribe/models` on Linux), then `./models` (so an rpg_vox checkout's models work as-is). In
TUI mode, logs go to `$TMPDIR/vox_scribe.log`.

On macOS, build with plain cargo (no nix-shell), and use `-p vox_scribe`:
the rpg_vox and discord_vox crates need PipeWire and only build on Linux.
sherpa-onnx is linked statically, so the binary is self-contained on both
macOS and Linux:

```bash
cargo build -p vox_scribe --release   # target/release/scribe
cargo install --path crates/vox_scribe   # or install into ~/.cargo/bin
```

## Install

```bash
just install-scribe              # or: just install-scribe /usr/local/bin
```

This builds the release binary, copies it to `~/.local/bin/scribe` (removing an old
`vox_scribe` binary there), and
downloads the models into the per-user data dir (skipped if already there).
Make sure the install directory is on your `PATH`. On macOS it also installs
`scribe-once.terminal` next to the binary and drops the Karabiner rule
below into `~/.config/karabiner/assets/complex_modifications/`.

## One-shot dictation key

`scribe --once` works as a push-to-talk dictation tool: bind a key that
opens a terminal running it, speak, press `enter`, and the text is on the
clipboard when the window closes.

### macOS (🎤 / F5 key)

macOS can't rebind the dictation key itself, so this uses
[Karabiner-Elements](https://karabiner-elements.pqrs.org/).

1. Run `just install-scribe`.
2. In System Settings › Keyboard › Dictation, turn Dictation off (or give
   it another shortcut). Otherwise macOS takes the key first.
3. Install Karabiner-Elements and allow its driver and Input Monitoring in
   System Settings › Privacy & Security.
4. In Karabiner-Elements › Complex Modifications › Add predefined rule,
   enable "Mic/F5 key: start scribe --once, or finish the running one".
5. Press 🎤 to start and again (or `enter`) to finish. The first time,
   allow Ghostty (or Terminal) to use the microphone. The window closes on its own.

Notes:

* The rule ([contrib/macos/karabiner-scribe.json](contrib/macos/karabiner-scribe.json))
  matches `f5` (what the built-in keyboard reports before Karabiner's
  function-key mapping) and `dictation` (external Apple keyboards).
  `fn`+F5 still sends a plain F5. If your key reports something else in
  Karabiner-EventViewer, edit the `from` entries.
* It runs `scribe stop-once || <open a terminal>`: a second press
  finishes the running recorder without opening a window.
* If Ghostty is installed, the recorder opens in a new Ghostty instance
  via [scribe-ghostty](contrib/macos/scribe-ghostty): a 100×20
  window at 13 pt, centred on the current screen. Change the size by
  setting `VOX_SCRIBE_FONT_SIZE=11` in front of it in the Karabiner rule.
  As a separate app, only its window comes forward; activating Terminal brings all of
  Terminal's windows to the front. Without Ghostty, or with
  `VOX_SCRIBE_TERMINAL=terminal just install-scribe`, it uses Terminal.
  Either way, the first launch asks to let that app use the microphone.
* For Terminal, [scribe-once.terminal](contrib/macos/scribe-once.terminal) is a
  Terminal window-settings file that runs scribe directly, so no login
  shell (and its `.zprofile`) runs first, and closes the window on exit.
  The mic opens before the models load, so you can start talking as soon
  as the window appears.
* It opens Terminal with `open -a Terminal` rather
  than `osascript … do script`. With osascript, macOS attributes the mic to
  Karabiner, which has no microphone permission, and scribe hears
  silence (the meter sits at -90 dB).
* Once Karabiner grabs the keyboard, System Settings › Keyboard › Modifier
  Keys no longer applies to it. Recreate remaps such as Caps Lock → Control
  in Karabiner-Elements › Simple Modifications.

### Linux

No special permissions are needed: PipeWire gives the mic to anything in
your session. Copying needs `wl-copy` (wl-clipboard) on Wayland or `xclip`
on X11. Bind any key to a command that finishes a running recorder or else opens
your terminal running `scribe --once`, e.g.
`sh -c 'scribe stop-once || foot scribe --once'` (without the
`stop-once` part a second press briefly opens a terminal that does the
same thing); use `wev` (Wayland) or `xev` (X11) to find the key's
name. Many laptops' mic keys are `XF86AudioMicMute`, which you may want to
keep; `F5` or `Super+D` are good alternatives.

* GNOME: Settings › Keyboard › View and Customize Shortcuts › Custom
  Shortcuts, command `gnome-terminal -- scribe --once` (or
  `ptyxis -- scribe --once`, `kgx -e scribe --once`).
* KDE Plasma: System Settings › Keyboard › Shortcuts › Add New › Command,
  `konsole -e scribe --once`.
* Sway / i3: `bindsym $mod+d exec foot scribe --once` (i3:
  `exec alacritty -e scribe --once`).
* Hyprland: `bind = SUPER, D, exec, kitty scribe --once`.

Desktop launchers often don't read your shell's `PATH`, so if nothing
happens use the full path, e.g. `~/.local/bin/scribe` (or
`/home/you/.local/bin/scribe` where `~` isn't expanded).

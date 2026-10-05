# scribe reference

Every command, option, environment variable and key of
[scribe](SCRIBE.md). For what scribe does and how to install it, start
with [SCRIBE.md](SCRIBE.md).

* [Command line reference](#command-line-reference)
* [Keys](#keys)
* [One-shot dictation key](#one-shot-dictation-key)

## Command line reference

```
scribe [OPTIONS] [COMMAND]
```

### Commands

| Command           | What it does                                                                                         |
|-------------------|------------------------------------------------------------------------------------------------------|
| *(none)*          | Transcribe (the TUI, or headless with `--no-tui` / `--input`).                                       |
| `devices`         | List input devices for the native audio backend.                                                     |
| `apps`            | List apps whose audio `--app` can capture; ones playing now are marked `*`.                          |
| `download-models` | Download the Zipformer model and the `--model` recognizer, plus the speaker models with `--diarize`. |
| `stop-once`       | Finish the running `--once` recorder, as if Enter were pressed. Exits 1 if none is running.          |
| `help`            | Print help, or the help of a subcommand.                                                             |

### Input

| Option              | Description                                                                                                                                                                                                                                               |
|---------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `-d, --device NAME` | Input device by id or name substring (see `devices`). Default: the system input. On Linux, a sink's node name captures what is playing on it. Repeatable: see [one source per speaker](SCRIBE.md#one-source-per-speaker).                                          |
| `-a, --app NAME`    | Transcribe what one app is playing: a PID, or a substring of its process name or bundle id (see `apps`). The app keeps playing to your speakers. macOS 14.4+ or PipeWire. Repeatable, and combines with `-d`. Conflicts with `--virtual-sink`, `--input`. |
| `--virtual-sink`    | PipeWire only: register a `vox_scribe` sink and transcribe whatever apps play into it.                                                                                                                                                                    |
| `-i, --input FILE`  | Transcribe a wav, flac, mp3 or ogg file instead of a device. Repeat it for per-speaker tracks of one session (all starting at the same moment). No TUI unless `--live`. Conflicts with `-d`, `--virtual-sink`.                                            |
| `--live`            | With `--input`: open the TUI and play the file to the speakers, transcribing it in real time as if it came from the mic. Times stay in file time; no seeking. Conflicts with `--no-tui`.                                                                  |

### Output

| Option              | Description                                                                                                                                                       |
|---------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `-o, --output NAME` | Write the transcript to `NAME.md` (`.md` is added if missing). Without `-o` or `-r` nothing is saved to disk (but `s` in the TUI can start a file).               |
| `-r, --record NAME` | Record a session: `NAME.md`, `NAME.srt`, `NAME.ass` and `NAME.opus`. See [output files](SCRIBE.md#output-files). Conflicts with `-o`, `--append`.                          |
| `-p, --play NAME`   | Play a recording made with `-r`: the audio with the subtitles shown word by word. Conflicts with every input and output option, `--once` and `--no-tui`.          |
| `--append`          | Add to an existing `-o` file, after a `---` rule and a new date line. The TUI shows the file's earlier content in grey. Without it, an existing file is an error. |
| `--title TEXT`      | Heading for the markdown file.                                                                                                                                    |
| `--no-tui`          | Print finished paragraphs to stdout instead of the full-screen UI. Always the case with `--input` (unless `--live`), which shows brief progress on stderr.        |

### Paragraphs and dictation

| Option     | Description                                                                                                                                                                                                                                                                                                                            |
|------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `--manual` | Start new paragraphs only on Enter, not on silence or word count. Toggle in the TUI with `m`; the header shows `¶ AUTO` or `¶ MANUAL`.                                                                                                                                                                                                 |
| `--once`   | One-shot dictation: record a single manual paragraph until Enter, finish every pass (including `--llm`), print the text, copy it to the clipboard and exit. Nothing is saved to disk. Only one runs at a time: launching a second one (or `scribe stop-once`) finishes the running one instead. Conflicts with `-o`, `--append`, `-r`. |

### Recognition

| Option                         | Default                             | Description                                                                                                                                                                                                                                                                                                  |
|--------------------------------|-------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `--model parakeet\|sensevoice` | `parakeet` (env `VOX_SCRIBE_MODEL`) | Recognizer for passes 2 and 3. Parakeet (NVIDIA Parakeet TDT 0.6B v2) is English only and noticeably more accurate on English. SenseVoice Small is multilingual, about twice as fast, uses half the memory (~620 MB vs ~1.2 GB on an M1) and is a smaller download (155 MB vs 460 MB, fetched on first use). |
| `--language LANG`              | `auto`                              | SenseVoice only: `auto`, `en`, `zh`, `ja`, `ko` or `yue`.                                                                                                                                                                                                                                                    |
| `--threads N`                  | `2`                                 | Threads per recognizer.                                                                                                                                                                                                                                                                                      |
| `--no-streaming`               | off                                 | Skip pass 1: no live partials, text appears per utterance.                                                                                                                                                                                                                                                   |
| `--vocab FILE`                 |                                     | Names and terms the recognizer can't know, one per line, `#` for comments. See [vocabulary](SCRIBE.md#vocabulary-optional).                                                                                                                                                                                           |
| `--models-dir DIR`             | (env `VOX_SCRIBE_MODELS`)           | Directory holding the model bundles (`sense-voice/`, `parakeet-tdt-0.6b-v2/`, `streaming-zipformer/`). See [models](SCRIBE.md#models-and-logs).                                                                                                                                                                       |

### LLM proofreading

| Option               | Default | Description                                                                                          |
|----------------------|---------|------------------------------------------------------------------------------------------------------|
| `--llm`              | off     | Enable pass 4. Configured by environment variables; see [pass 4](SCRIBE.md#pass-4-llm-proofreading-optional). |
| `--llm-timeout SECS` | `30`    | How long to wait for each pass-4 reply before keeping the pass-3 text.                               |

### Speakers

| Option                  | Default | Description                                                                                                                                                                                                                  |
|-------------------------|---------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `--diarize`             | off     | Label who is speaking (Speaker A, B, …), live and, with `-r` or `-i … -o`, with a full diarization after the session. Fetches ~77 MB of speaker models on first use.                                                         |
| `--speakers N\|NAMES`   |         | Who is speaking, when known: a count (`3`) or names in the order they are first heard (`Alice,Bob,Carol`, which also means 3). The count is exact. With several sources, their names in the order the sources were given.    |
| `--speaker-threshold X` | `0.7`   | Needs `--diarize`. How alike (cosine similarity, 0–1) a voice must be to a known speaker to get their live label. Raise it if two people share a label, lower it if one person is split in two. Only the live labels use it. |

### Logging

| Option          | Description                                                                                                                                                                       |
|-----------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `--log FILE`    | Log file. Default: `$TMPDIR/vox_scribe.log` in TUI mode, stderr otherwise. The level is set with `VOX_SCRIBE_LOG` (a `tracing` filter such as `debug` or `vox_transcribe=trace`). |
| `-h, --help`    | Print help (`-h` for the short form).                                                                                                                                             |
| `-V, --version` | Print the version.                                                                                                                                                                |

### Environment variables

| Variable                                      | Used for                                                |
|-----------------------------------------------|---------------------------------------------------------|
| `VOX_SCRIBE_MODELS`                           | Same as `--models-dir`.                                 |
| `VOX_SCRIBE_MODEL`                            | Same as `--model`.                                      |
| `VOX_SCRIBE_LLM_URL`                          | Pass-4 endpoint, default `https://api.openai.com/v1`.   |
| `VOX_SCRIBE_LLM_MODEL`                        | Pass-4 model name (required with `--llm`).              |
| `VOX_SCRIBE_LLM_KEY`, `OPENAI_API_KEY`        | Pass-4 API key, optional for local servers.             |
| `VOX_SCRIBE_LOG`                              | Log filter.                                             |
| `VOX_SCRIBE_TERMINAL`, `VOX_SCRIBE_FONT_SIZE` | macOS dictation key setup; see [below](#macos--f5-key). |

## Keys

Esc can also be typed as `ctrl+[` (and Emacs vterm's Alt+Ctrl+[ works).

### Recording TUI

| Key                    | Action                                                                                                                                                                                              |
|------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `↑` / `k`              | Select the previous paragraph.                                                                                                                                                                      |
| `↓` / `j`              | Select the next paragraph.                                                                                                                                                                          |
| `y` / `c`              | Copy the selected paragraph to the clipboard (the newest one if none is selected).                                                                                                                  |
| `PgUp` / `PgDn`        | Scroll back / forward 10 lines (clears the selection).                                                                                                                                              |
| `End` / `G`            | Jump to the bottom and follow new text.                                                                                                                                                             |
| `space`                | Pause / resume. Pausing ends the current paragraph and releases the mic, so the OS stops showing it in use; timestamps keep running. With `--live`, the file's playback pauses too.                 |
| `enter`                | Start a new paragraph.                                                                                                                                                                              |
| `m`                    | Switch between auto paragraphs (silence or word count breaks them) and manual ones (only `enter` does).                                                                                             |
| `s`                    | Start saving: prompts for a file name (default `transcript-YYYYMMDD-HHMMSS.md` in the current directory), writes the settled paragraphs so far and appends from then on. Says so if already saving. |
| `o`                    | With `--llm`: toggle showing the text from before pass 4.                                                                                                                                           |
| `n`                    | With `--diarize` or several sources: open the speaker list.                                                                                                                                         |
| `a`                    | Without -d / -a / -i: open the audio source list to switch inputs or apps, or add more.                                                                                                             |
| `q` / `esc` / `ctrl+c` | Quit. Whatever paragraph is still open is finished and written first.                                                                                                                               |

In the **audio source list**: `↑`/`k` and `↓`/`j` move, `space` ticks
or unticks an input or app (apps playing sound are listed first),
`enter` switches to what's ticked, `esc` / `a` / `q` closes. Ticking
several transcribes them at once, each its own speaker, as with repeated
`-d` / `-a`. A source added mid-session starts at the current time and
goes into the `--record` mix; one that's unticked goes quiet but keeps
its speaker label, which a newly ticked source takes over. With
`--diarize`, only one source can be picked.

In the **save prompt**: type the name, `backspace` deletes, `enter` saves
(an existing file is refused), `esc` cancels.

In the **speaker list**: `↑`/`k` and `↓`/`j` select, `enter` types a
new name (then `enter` to apply, `esc` to cancel; an empty name keeps the
old one), `del` / `backspace` restores "Speaker A", `esc` / `n` / `q`
closes. Renaming applies on screen straight away; files already written
get the new names when the session ends.

### `--once` (dictation)

| Key                             | Action                                                                                                                                                                       |
|---------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `enter`                         | Finish: run the remaining passes, copy the text and exit.                                                                                                                    |
| `shift+enter`                   | Start a new paragraph without finishing. Needs a terminal with the kitty keyboard protocol (Ghostty, kitty, WezTerm, iTerm2); macOS Terminal sends the same code as `enter`. |
| `space`                         | Pause / resume.                                                                                                                                                              |
| `↑` `↓` `y` `PgUp` `PgDn` `End` | As in the recording TUI.                                                                                                                                                     |
| `q` / `esc` / `ctrl+c`          | Cancel without copying.                                                                                                                                                      |

`m`, `n` and `s` are off in this mode. With `--once --no-tui`, a line on
stdin (`enter`) finishes and `ctrl+c` cancels.

### `--play` (playback)

| Key            | Action                                                                                                                                                                                |
|----------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `space`        | Pause / resume (at the end, restart from the top).                                                                                                                                    |
| `←` / `→`      | Jump to the previous / next word.                                                                                                                                                     |
| `↑` / `↓`      | Jump to the previous / next line (subtitle cue).                                                                                                                                      |
| `home`         | Restart from the beginning.                                                                                                                                                           |
| `<` / `,`      | Slow down, in steps 0.5×, 0.75×, 1×, 1.25×, 1.5×, 1.75×, 2×, 2.5×, 3×, without changing the pitch.                                                                                    |
| `>` / `.`      | Speed up. The status bar shows the speed.                                                                                                                                             |
| `/`            | Search, like `less`: type a pattern (case-insensitive, may span words), `enter` jumps to the next match, `esc` / `ctrl+c` cancels, `backspace` on an empty pattern closes the prompt. |
| `n`            | During a search: next match. Otherwise: open the speaker list to rename a speaker in `NAME.md`, `.srt` and `.ass`.                                                                    |
| `p` / `N`      | Previous match. Matches wrap at the ends and are underlined.                                                                                                                          |
| `esc`          | Clear the search; with no search, quit.                                                                                                                                               |
| `q` / `ctrl+c` | Quit.                                                                                                                                                                                 |

The speaker list works as in the recording TUI, except that renames are
written to the files immediately.

### Headless (`--no-tui`, `--input`)

The first `ctrl+c` ends a live session gracefully (the open paragraph is
finished and written); a second one quits at once. With `--input` there
is nothing to stop gracefully, so `ctrl+c` quits straight away. During the
final diarization (which runs on the console after the TUI closes too),
`ctrl+c` skips it and the files keep the live speaker labels.

## One-shot dictation key

`scribe --once` works as a push-to-talk dictation tool: bind a key that
opens a terminal running it, speak, press `enter`, and the text is on the
clipboard when the window closes. A second press of the same key can run
`scribe stop-once` to finish it.

### macOS (🎤 / F5 key)

macOS can't rebind the dictation key itself, so this uses
[Karabiner-Elements](https://karabiner-elements.pqrs.org/).

We recommend installing [Ghostty](https://ghostty.org) first. The
recorder then pops up as a small window over whatever app you're
dictating into, and only that window comes forward. With macOS Terminal,
every Terminal window comes to the front with it. Ghostty also tells
`shift+enter` apart from `enter`, so you can start a new paragraph
without finishing.

1. Run `just install-scribe`. It uses Ghostty if it's installed.
2. In System Settings › Keyboard › Dictation, turn Dictation off (or give
   it another shortcut). Otherwise macOS takes the key first.
3. Install Karabiner-Elements and allow its driver and Input Monitoring
   in System Settings › Privacy & Security.
4. In Karabiner-Elements › Complex Modifications › Add predefined rule,
   enable "Mic/F5 key: start scribe --once, or finish the running one".
5. Press 🎤 to start and again (or `enter`) to finish. The first time,
   allow Ghostty (or Terminal) to use the microphone. The window closes
   on its own.
6. To transcribe an app's audio there (with `a`), also allow Ghostty (or
   Terminal) under *System Audio Recording Only* in System Settings ›
   Privacy & Security › Screen & System Audio Recording. Without it, the
   app records as silence.

Notes:

* The rule ([karabiner-scribe.json](crates/vox_scribe/contrib/macos/karabiner-scribe.json))
  matches `f5` (what the built-in keyboard reports before Karabiner's
  function-key mapping) and `dictation` (external Apple keyboards).
  `fn`+F5 still sends a plain F5. If your key reports something else in
  Karabiner-EventViewer, edit the `from` entries.
* It runs `scribe stop-once || <open a terminal>`: a second press
  finishes the running recorder without opening a window.
* If Ghostty is installed, the recorder opens in a new Ghostty instance
  via [scribe-ghostty](crates/vox_scribe/contrib/macos/scribe-ghostty): a
  100×20 window at 13 pt, centred on the current screen. Change the size
  by setting `VOX_SCRIBE_FONT_SIZE=11` in front of it in the Karabiner
  rule. As a separate app, only its window comes forward; activating
  Terminal brings all of Terminal's windows to the front. Without
  Ghostty, or with `VOX_SCRIBE_TERMINAL=terminal just install-scribe`, it
  uses Terminal. Either way, the first launch asks to let that app use
  the microphone.
* For Terminal, [scribe-once.terminal](crates/vox_scribe/contrib/macos/scribe-once.terminal)
  is a Terminal window-settings file that runs scribe directly, so no
  login shell (and its `.zprofile`) runs first, and closes the window on
  exit. The mic opens before the models load, so you can start talking as
  soon as the window appears.
* It opens Terminal with `open -a Terminal` rather than
  `osascript … do script`. With osascript, macOS attributes the mic to
  Karabiner, which has no microphone permission, and scribe hears silence
  (the meter sits at -90 dB).
* Once Karabiner grabs the keyboard, System Settings › Keyboard ›
  Modifier Keys no longer applies to it. Recreate remaps such as
  Caps Lock → Control in Karabiner-Elements › Simple Modifications.

### Linux

No special permissions are needed: PipeWire gives the mic to anything in
your session. Copying needs `wl-copy` (wl-clipboard) on Wayland or
`xclip` on X11. Bind any key to a command that finishes a running
recorder or else opens your terminal running `scribe --once`, e.g.
`sh -c 'scribe stop-once || foot scribe --once'` (without the
`stop-once` part a second press briefly opens a terminal that does the
same thing); use `wev` (Wayland) or `xev` (X11) to find the key's name.
Many laptops' mic keys are `XF86AudioMicMute`, which you may want to
keep; `F5` or `Super+D` are good alternatives.

* GNOME: Settings › Keyboard › View and Customize Shortcuts › Custom
  Shortcuts, command `gnome-terminal -- scribe --once` (or
  `ptyxis -- scribe --once`, `kgx -e scribe --once`).
* KDE Plasma: System Settings › Keyboard › Shortcuts › Add New ›
  Command, `konsole -e scribe --once`.
* Sway / i3: `bindsym $mod+d exec foot scribe --once` (i3:
  `exec alacritty -e scribe --once`).
* Hyprland: `bind = SUPER, D, exec, kitty scribe --once`.

Desktop launchers often don't read your shell's `PATH`, so if nothing
happens use the full path, e.g. `~/.local/bin/scribe` (or
`/home/you/.local/bin/scribe` where `~` isn't expanded).

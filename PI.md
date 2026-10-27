# Talking to Pi

`pi/scribe-voice.ts` is an extension for the [Pi](https://github.com/earendil-works/pi)
coding agent that turns it into a voice conversation: what you say is
transcribed live by [scribe](SCRIBE.md) and sent when you stop talking,
and replies are read aloud by a local Qwen3-TTS voice, appearing on
screen as they are spoken. macOS (Apple Silicon) only, for the voice.

```bash
just install-scribe     # scribe with --jsonl, on your PATH
just pi-tts-venv        # Python venv with mlx-audio, and the voice model (2.2 GB)
just pi                 # pi with the extension
```

## Using it

Pi starts listening as soon as the models are loaded (the status line
says so). Talk, and once you pause for a moment what you said is sent.
When the reply has been read out, the mic opens again for a follow-up.

| Key / command | What it does |
|---|---|
| Space | Pause the mic, and again to resume. During a reply: interrupt it and talk. |
| Enter | Send now. |
| Backspace | Delete the last dictated word. |
| Escape | Drop what is still being transcribed and pause. During a reply: interrupt it and stop the agent. |
| typing | Edits the text; new speech is added after it. Once you have typed something, Space and Backspace type as usual until it is sent. |
| `/voice` | Voice mode, listening, with the last voice and style. Prints them, the voices and how to use the command. |
| `/voice NAME [style]` | Pick a voice and how it speaks: `/voice ryan calm and warm, a little playful`. `/voice ryan -` speaks plainly. |
| `/text` | Back to typing: the mic is off, replies aren't read, and both models are unloaded. |

The mode, voice and style are kept in `~/.pi/agent/scribe-voice.json`.

A reply is shown only as far as it has been said. Interrupting it (Space
or Escape) keeps it in the conversation only up to the last word you
heard, followed by ` —`, and drops any tools it was about to run, so the
model knows where it was cut off. Each message waits until it has been
spoken before the agent goes on.

## Pieces

- `scribe --jsonl --standby` runs for the whole Pi session and opens the
  mic only while you dictate. Its protocol (`start`, `pause`, `finish`,
  `set_text`, `delete_word`, …) is in the
  [reference](SCRIBE_REFERENCE.md#--jsonl-dictation-over-a-pipe).
- `pi/pi-tts.py` speaks sentences it is sent over stdin with
  [mlx-audio](https://github.com/Blaizzy/mlx-audio) and
  `mlx-community/Qwen3-TTS-12Hz-1.7B-CustomVoice-4bit`, streaming, and
  reports how far it has played each one so the text can keep pace.
  About 5.3 GB of memory at peak.

## Environment

| Variable | Default |
|---|---|
| `SCRIBE_BIN` | `scribe` on your PATH |
| `SCRIBE_ARGS` | extra scribe arguments, e.g. `--paragraph-gap 1` |
| `SCRIBE_AUTO_FINISH_MS` | `1500`: quiet that sends what you said |
| `PI_TTS_PYTHON` | `pi/.venv/bin/python`, else `~/tts-spike/.venv/bin/python` |
| `PI_TTS_MODEL` | `mlx-community/Qwen3-TTS-12Hz-1.7B-CustomVoice-4bit` |
| `PI_TTS_VOICE`, `PI_TTS_INSTRUCT` | the voice and style before `/voice` has picked any |
| `PI_TTS` | `0` starts in text mode until `/voice` is used |
| `SCRIBE_VOICE_SETTINGS` | `~/.pi/agent/scribe-voice.json` |

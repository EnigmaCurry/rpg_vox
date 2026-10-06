# rpg_vox

![rpg_vox](rpg_vox.png)

A collection of voice tools for tabletop RPGs played over a video
call, as well as for general transcription tasks, built on PipeWire
and local speech models. This repository holds two separate apps built
with the same core, plus an LLM responder for scribe's chat mode:

> [!NOTE]
> These tools are produced with the assistance of Claude Code and other AI tools. 

## [scribe](SCRIBE.md)

A standalone live transcription app for the terminal, with nothing
RPG-specific in it. It shows what is being said as it is said, refines
the text in the same layered passes as rpg_vox, and saves it as
markdown, optionally with subtitles, audio and speaker labels. It also
works as a one-key dictation tool. macOS and Linux.

```bash
just install-scribe
scribe -o notes
```

[Read more →](SCRIBE.md)

## [agent](AGENT.md)

The other side of `scribe --chat`: answers what you say with any
OpenAI-compatible LLM, streaming each reply back a sentence at a time
for scribe to read aloud. You can talk over it to cut it off.

```bash
just install-agent
agent talk & scribe --chat talk
```

[Read more →](AGENT.md)

## [rpg_vox](RPG_VOX.md)

status: **EXPERIMENTAL**

An audio bridge and chat bot for running a game on Discord (or any voice
setup PipeWire can reach). It voices your characters with text-to-speech
through a virtual mic, helps write their lines with an LLM, and
transcribes the table live as you play. A companion bot, discord_vox,
carries the audio to and from a Discord voice channel. Linux only.

[Read more →](RPG_VOX.md)

## License

MIT, see [LICENSE.txt](LICENSE.txt).

# rpg_vox

![rpg_vox](rpg_vox.png)

A collection of voice tools for tabletop RPGs played over a video
call, as well as for general transcription tasks, built on PipeWire
and local speech models. This repository holds several separate apps built
with the same core. 

> [!NOTE]
> These tools are produced with the assistance of Claude Code and other AI tools. 

## [scribe](SCRIBE.md)

A standalone live transcription app for the terminal, with nothing
RPG-specific in it. It shows what is being said as it is said, refines
the text in the same layered passes as rpg_vox, and saves it as
markdown, optionally with subtitles, audio and speaker labels. It also
works as a one-key dictation tool. macOS, Linux and Windows.

```bash
just install-scribe
scribe -o notes
```

[Read more →](SCRIBE.md)

## [Scribe for Android](SCRIBE_ANDROID.md)

scribe as a phone app: live transcription of the mic, other apps'
audio, or shared files, kept as threads, plus a voice keyboard for
dictating into any app. Everything runs on the phone after a one-time
model download. Arm64, Android 8.0 or newer.

[Read more →](SCRIBE_ANDROID.md)

## [Scribe for iOS](IOS.md)

scribe as an iPhone and iPad app (work in progress): live
transcription of the mic or shared audio and video files, dictations
you can keep editing by voice, threads, and playback of kept audio.
Everything runs on the device after a one-time model download. iOS 17
or newer, built and installed from source for now.

[Read more →](IOS.md)

## [agent](AGENT.md)

The other side of `scribe --chat`: answers what you say with any
OpenAI-compatible LLM, streaming each reply back a sentence at a time
for scribe to read aloud. You can talk over it to cut it off.

```bash
just install-agent
agent talk & scribe --chat talk
```

[Read more →](AGENT.md)

## [Pi voice](PI.md)

An extension for the Pi coding agent that turns it into a voice
conversation: scribe hears you and sends what you said when you pause,
and a local Qwen3-TTS voice reads the replies, shown as they are
spoken. Talk over it to cut it off. macOS, or Linux with a Qwen3-TTS
server for the voice.

```bash
just pi-tts-venv
just pi
```

[Read more →](PI.md)

## [Voice room](ROOM.md)

A voice chat in the browser for a few people and an AI. Everyone joins
with an invite link and a headset, hears everyone else and the AI, and
sees one live transcript of who said what. The AI answers out loud and
can be interrupted just by talking; the log keeps only what it actually
said. macOS and Linux.

One server holds several rooms, and you move between them without the
audio stopping — the house's rooms, and one of your own where you can
talk to the AI with nobody listening.

```bash
just room-invite Ryan
just room --room Lounge --room Studio
```

[Read more →](ROOM.md)

## [Voice room for Android](ROOM_ANDROID.md)

The room as a phone app, so a call survives the screen going off the
way a phone's own calls do. It is a wrapper and says so: the whole of
it is a WebView and a foreground service, and the room inside is the
same room a browser gets.

It ships no address. Whose room it is belongs to whoever installed it,
so the first screen asks and the answer is kept; after that the app
remembers the room rather than the link, because a link is spent the
moment it works. Android 11 or newer, one APK for every phone.

[Read more →](ROOM_ANDROID.md)

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

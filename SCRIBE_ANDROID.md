# Scribe for Android

[scribe](SCRIBE.md)'s live transcription as an Android app. Words
appear as they are spoken and are corrected when you pause, using the
same speech models and passes as the desktop version, all running on
the phone. It also adds a voice keyboard for dictating into other apps.

* **On the phone, offline.** After a one-time model download, nothing
  leaves the device.
* **Mic, phone audio, or both.** Transcribe yourself, whatever another
  app is playing (a call, a video), or both at once with each paragraph
  labelled Me or Phone.
* **Files.** Share an audio or video file to Scribe, or open one with
  it, and it is transcribed faster than real time.
* **Threads.** Each transcript is a thread you can rename, reopen and
  add to later.
* **Keeps going in the background.** Recording continues with the
  screen off, with Pause/Resume and Stop in the notification.
* **Voice typing.** A Scribe keyboard that listens as soon as it opens
  and types the finished text into any app.
* **In English or Russian.** The app's own buttons, settings and
  notifications follow the phone's language, and can be set to one of
  their own on Android 13 and newer. Which language is *transcribed* is
  a separate matter, and its own setting.
* **Reads another language in yours.** A paragraph heard in Russian is
  shown in English, on the phone, with nothing asked of a network. The
  transcript is in the language you read and in no other; what was said
  is kept all the same, and is what every pass and every reading is of.
  Eighteen languages to read in English, in any alphabet, including
  languages written like your own.
* **Models you manage.** One page lists every model with its size and
  whether it is on the phone, and lets you fetch or delete any of them.

Arm64 phones running Android 8.0 (API 26) or newer. The finished text
covers 25 European languages; the live text, as you speak, is English or
Russian, and [follows whichever is being spoken](#languages).

* [Install](#install)
* [First run: speech models](#first-run-speech-models)
* [Recording](#recording)
* [Threads](#threads)
* [Playing back](#playing-back)
* [Transcribing a file](#transcribing-a-file)
* [Settings](#settings)
* [The app's own language](#the-apps-own-language)
* [Languages](#languages)
* [Models](#models)
* [What the transcript shows](#what-the-transcript-shows)
* [Voice typing keyboard](#voice-typing-keyboard)
* [Building from source](#building-from-source)

## Install

### With Obtainium (recommended)

[Obtainium](https://github.com/ImranR98/Obtainium) installs apps
straight from their GitHub releases and updates them when a new one is
published.

1. Install Obtainium.
2. On the phone, open
   [Add Scribe to Obtainium](https://apps.obtainium.imranr.dev/redirect?r=obtainium://add/https://github.com/EnigmaCurry/rpg_vox),
   or tap **Add App** in Obtainium and enter
   `https://github.com/EnigmaCurry/rpg_vox`.
3. In the app's settings, put this in **Custom APK link filter by
   regular expression**:

   ```
   scribe-.*-android-arm64\.apk$
   ```

4. Tap **Add**, then **Install**. Android asks you to allow installs
   from Obtainium the first time.

A release carries two APKs now, scribe's and [the voice
room's](ROOM_ANDROID.md), so without that filter Obtainium asks **Pick
an APK** every time one is published, and the wrong answer installs the
other app. The filter is what makes the choice once. The desktop
archives are not `.apk` and were never in the running.

### By hand

Download `scribe-<version>-android-arm64.apk` from the
[releases page](https://github.com/EnigmaCurry/rpg_vox/releases) on the
phone and open it. Android asks you to allow installs from your browser
or file manager the first time. Repeat for each new version.

If a new version won't install over the old one, the two were signed
with different keys: uninstall Scribe first. This deletes its threads
and downloaded models.

## First run: speech models

Scribe needs two models from the
[sherpa-onnx releases](https://github.com/k2-fsa/sherpa-onnx/releases):
a streaming Zipformer for the live text and Parakeet TDT 0.6B v3 for the
corrected text. Tap **Download models** to fetch them, about 615 MB in
all, into the app's private storage. Wi-Fi is recommended. The download
continues if you switch apps, and if it fails, **Retry** resumes where it
stopped.

The models stay loaded while you use Scribe and are unloaded to free
memory after it has been out of sight for 10 minutes. Tap **Load
models** to load them again.

Everything else is fetched only if a setting asks for it, and all of
it can be managed from [**Settings → Models**](#models):

| Model | What for | Download |
| --- | --- | --- |
| Streaming Zipformer (English) | the live text, in English | 128 MB |
| Parakeet TDT 0.6B v3 | the finished text, 25 languages | 487 MB |
| Vosk streaming Russian | the live text, in Russian | 27 MB |
| fastText lid.176 | telling languages apart | 917 KB |
| opus-mt, one per direction | reading another language in yours | 81 MB each |

## Recording

Pick a source at the top:

* **Mic**: your voice.
* **Phone**: audio other apps are playing. Android asks for permission
  to capture the screen each time you start, since that is how apps
  reach other apps' audio. Stopping the capture from the system's
  notification stops the recording.
* **Both**: the mic and the phone audio as separate speakers, Me and
  Phone, so you can transcribe both sides of a call.

Then tap the record button. The level meter shows what the mic hears.
Faint text is the live guess, which is replaced by the corrected
paragraph when you pause.
While recording:

* **❚❚ / ▶** pauses and resumes (in **Clear**'s place, which returns
  once the recording stops).
* The transcript follows the newest text. Scroll up to read back, and
  **↓ Jump to latest** returns.

**Copy** and **Share** send the whole thread as text, and **Clear**
empties it. Scribe asks for the notification permission on first
recording so the Pause/Resume and Stop controls can appear there.

Recording carries on while you are in another app, but not at the same
speed. Android gives the app on screen the phone's fast cores and an app
in the background the slow ones, and every pass runs on the CPU here —
so the words keep coming and the passes behind them fall back. On a
Pixel 6, transcribing a video playing in another app, pass 2 ran about a
third slower off screen and the reading of another language ran at
roughly half the speed of the speech, so it was minutes behind by the
time the recording stopped. It all catches up — stopping, or coming back
to Scribe, drains the backlog in seconds — and the status line says
"reading N paragraphs behind" while it is behind. For a long recording
in another language, leaving Scribe on screen (the other app in
picture-in-picture, or split screen) is the difference between a reading
that keeps up and one that arrives at the end.

## Threads

Transcripts are kept as threads. **☰** opens the list, most recent
first, with **New thread** at the top. Tap a thread to open it, or
long-press it to rename or delete it. Tapping the title renames the open
thread. A new thread is named with today's date, and gains the time
when you press record — so its name is when you spoke, not when you
made it. Renaming it, or saying anything in it, settles the name for
good.

Recording adds to the open thread. While a recording or a file is being
transcribed, the other threads can't be opened.

## Playing back

With **Keep audio (.opus)** on in **⚙** → **Recording**, tap any word of a
transcript, once the recording has stopped, to hear it from that word.
The words light up as they are spoken and the transcript scrolls along.
While it plays, **« 10s** and **10s »** (back and ahead ten seconds),
**Pause**/**Resume** and **Stop** take the place of the source picker,
with the time below. Tapping the transcript also pauses: the
current word stays highlighted and you can scroll freely until you
resume, or tap another word to play from there. Nothing else is
shown while it plays; **Stop** brings back recording. Playback carries on
with the app in the background or the screen off, with the same controls
in the notification and on the lock screen (and headset buttons). It
pauses for a call or another app's audio, and when headphones are
unplugged. A Both recording plays the mic and the
phone audio together.

Playback runs through all of a thread's recordings in order, one after
another, and the time shown is for the whole thread. The green ▶ above
a recording's first paragraph plays from its start; long-press it to
delete that one recording (its audio and its text). With **Keep non-speech audio** on, a recording in which
no words were heard appears as "▶ No words detected" where it happened,
or, in a thread with no text at all, as a big ▶ with "No words detected
in log"; it plays from its first sound, though the recording is kept
whole. Long-pressing still selects text to copy, and the selection menu also
has **Edit**, to retype that paragraph, and **Cut**, which removes the
selected words and skips their audio on playback (the recording itself
is kept whole). Cut also works while recording; Edit waits until the
recording stops. Paragraphs recorded without
the setting have no audio.

## Transcribing a file

Share an audio or video file to Scribe from another app (**Transcribe
with Scribe**), or open one with it from a file manager. Scribe starts a
new thread named after the file and transcribes it faster than real
time, with progress in the app and the notification.

## Settings

**⚙** lists the settings pages and About.

**Microphone**: changes apply the next time the mic starts, so pause and
resume to apply them mid-recording.

* **Mic**: **Bluetooth headset when connected** (the default) records
  from a connected headset's mic, and the status line names the input in
  use. **Phone mic always** ignores headsets.
* **Mic boost**: **Auto (raise quiet speech)** brings quiet speech up to
  a steady level without raising background noise into words. **Off**,
  **+6 dB**, **+12 dB** and **+18 dB** apply a fixed boost instead.
  Phone audio and files are never boosted.

**Recording**:

* **Keep audio (.opus)**: saves each recording's audio (and a shared
  file's) with its transcript, along with when each word was said, so
  you can [play it back](#playing-back). Off by default. The audio is
  deleted with its thread, or by **Clear**.
* **Keep non-speech audio**: with Keep audio on, also keeps recordings
  in which no words were detected. Off by default, so those are deleted
  when they finish, as is a recording once all its text has been cut. Playback
  also shortens any quiet between words to 1 second.

**Languages**: see [Languages](#languages) and [what the transcript
shows](#what-the-transcript-shows) below.

**Models**: every model, with what each is for and a button to fetch or
delete it. See [Models](#models).

**Keyboard**: a shortcut to turn on the [voice typing
keyboard](#voice-typing-keyboard).

**Display**: the transcript's font size, five steps with the default in
the middle.

**About**: the app's version, a link to its source, and the
[changelog](CHANGELOG.md).

## The app's own language

Scribe's buttons, settings, dialogs and notifications are in English or
Russian, and that is a different question from which language it
*transcribes* — for which see [Languages](#languages) below. You can
read the app in Russian and transcribe English in it, or the other way
round.

It follows the phone: whichever of the two comes first in **Settings →
System → Languages** is the one Scribe is in, and English is what it
falls back to for a phone in any other language. On **Android 13 and
newer** it can also be given a language of its own, without touching
the rest of the phone, under **Settings → Apps → Scribe → Language**
(or the same entry from Android's own languages page). Android keeps
that choice, and Scribe is restarted into it at once.

Three things stay in English whichever language is chosen, each for a
reason:

* The **changelog** in About. It is [CHANGELOG.md](CHANGELOG.md),
  shipped with the app as it was written.
* A **model's name** on the download lines ("parakeet-tdt-0.6b-v3"),
  which is what the release it comes from is called.
* An **error** the engine or the network reports, which is what a bug
  report needs quoted rather than what a reader needs to read.

A thread already recorded is never rewritten: a paragraph's speaker is
stored as `Me` or `Phone` and a reading remembers the language it was
heard in by its English name, so the words you see change with the
setting while the transcript itself does not. The voice keyboard's
built-in edit keys are a plain QWERTY row in either language, and the
rules for capitalising dictated text are English ones — both belong to
the language being *spoken*, which is a thing this setting does not
touch.

Adding a third language is a file of strings and a line of XML; see
[Translating](#translating) under Building from source.

## Languages

**Settings → Languages** has three things: the language you read in, the
languages to listen out for, and **Show what was said**, which is what a
paragraph shows while its reading catches up. The page says each of them
in a line; the rest of what they mean is here. Downloading the models
any of it needs is [its own page](#models).

**I read in** is the language every translation is read into, and the
one the words that appear *as you speak* start out in.

Those words come from pass 1, the one pass that has to be told which
language to listen in: it decodes as the words are said, so its model is
chosen before any of them have been, where passes 2 and 3 work the
language out from the audio in any of 25. You are not asked which it
will be. Pass 1 starts in the language you read in and then follows the
speaking: an utterance that comes back in another alphabet is enough for
the next one to be listened to in that language, and for it to go back
when the speaking does. Nothing changes within an utterance — the
decode is swapped between them, where it would otherwise lose the words
already on screen.

English and Russian are the two languages pass 1 has a model for. The
model for a language you turn on is downloaded with it (27 MB for
Russian); until it is there the live text cannot be in that language,
and the settings page and the recording status line say so rather than
listening in the wrong one without a word.

**Listen out for** is any number of languages, in any alphabets,
including your own. A paragraph heard in one of them gets a reading in
the language you read, a sentence at a time, on the phone. Each is one
model, about 81 MB, from **Settings → Models**.

Which languages are offered is the uploader's doing and nothing else:
they are the ones with a published model that reads into yours.

| You read | Listen out for |
| --- | --- |
| English | Bulgarian, Czech, Danish, Dutch, Estonian, Finnish, French, German, Hungarian, Italian, Latvian, Maltese, Polish, Russian, Slovak, Spanish, Swedish, Ukrainian; Chinese, Japanese, Korean |
| Russian | English, French, Spanish, Ukrainian; Korean |

That list used to be two or three languages, one per alphabet, and the
reason is worth keeping: the recognizer that hears 25 languages never
says which one it heard, so a paragraph's language is worked out from
its text afterwards. The alphabet does most of that for nothing and is
never wrong about it — Cyrillic is not Latin — but it cannot tell
French from the English it would be read into, and a wrong guess sends a
paragraph to a model that translates it out of a language it is not in,
which comes back as confident nonsense rather than as an error.

So the rest of the decision is a model too: **Telling languages apart**,
917 KB, on the Models page. It is asked only what the alphabet could not
settle, only about the languages you have turned on, and only its
answers above nine-tenths certainty are used; below that the paragraph
stands as it was said, which is what the alphabet alone used to do with
every one of them. Turning on two languages written the same way asks
for it, and until it is there that pair is refused rather than guessed
at — the settings page and the recording status line say so.

## Models

**Settings → Models** is every model: what it is for, how big it is,
whether it is on the phone, and a button to fetch or delete it. The
heading says how much they take altogether.

They are grouped by what they do rather than by where they came from:

* **The finished text** — Parakeet TDT 0.6B v3, 487 MB. Needed to run,
  so it has no Delete.
* **The live text, as you speak** — one per language, English (128 MB)
  and Russian (27 MB). The English one is needed to run too: it is
  fetched on the first run either way, and a sentence of English inside
  another language's conversation is the common case pass 1 changes
  language for.
* **Reading another language in yours** — **Telling languages apart**
  (917 KB), and one opus-mt model per direction (about 81 MB each).

A row says which of these a model is: **in use** by the languages you
have chosen, **no language uses it** for one left behind by a setting
you changed, or **a chosen language needs this** in red for one that is
missing — which is the state where nothing is being read. **Download
what's missing** at the top fetches all of those at once.

Anything but the two required models can be deleted and fetched again.
Deleting one a chosen language is using is allowed: that language simply
stops being read until it is back, which the row and the status line
say. Models live in the app's private storage and go when the app is
uninstalled.

The downloads are the same models, from the same sources, in the same
layout as the desktop [scribe](SCRIBE.md#models-and-logs), so a models
directory from one would work for the other.

## What the transcript shows

The language you read, first and by default. A paragraph spoken in
Russian is shown in English, a sentence at a time as each sentence is
read, and the Russian is not shown until you ask for it. A transcript
you can read is the point of translating it, so the reading is what the
paragraph is; asking is one tap, and nothing is hidden.

What was said is still what is *kept*. It is in the thread, it is what
every later pass refines, and it is what each reading is made from
again whenever one of those passes changes a word -- a sentence pass 3
rewrites is read again, so what you see is of the words as they finally
stand. It is simply not what is on the screen.

What was said is shown when it is *not known to be* in an alphabet you
don't read. That covers a paragraph pass 1 is still typing, which has no
verdict yet, and one whose alphabet could not be read at all — too few
letters, or too much of a mixture, which a stray `<unk>` manages — where
the engine's answer is the same as for a paragraph in your own language:
nothing to translate. The alphabet is asked here instead, by the same
test the engine uses.

Text too short or too mixed to place is shown: `OK.` is as likely as
`Да.`, and hiding every short utterance to catch the second would lose
the first. For those the engine assumes the language pass 1 is listening
in, so most of them are read rather than left to that guess.

Three more things follow:

* A reading says that it is one. It carries a rule down its left edge,
  which is the whole of the indicator — a paragraph with that rule is
  words nobody said, a paragraph without it is what somebody said — and
  over it, the language it was heard in: **From English**. Tapping that
  opens the transcript up sentence by sentence: each sentence as it was
  said, faintly, with its reading on the line under it. Tapping again
  closes it back to the readings alone.

  One switch for the whole thread, not one per paragraph. Any tag
  throws it and they all show the same chevron, because reading a
  translated thread against the original is a way of reading rather
  than a note taken on one paragraph, and opening each paragraph in
  turn to do it is the same tap over and over.

  Zipped rather than the reading and then the paragraph, because
  checking a reading means holding one sentence against one sentence,
  and down a long paragraph two blocks are too far apart to do it — by
  the foot of the reading there is nothing left on screen to check it
  against. A sentence the translator hasn't reached stands alone, with
  nothing under it yet. Tapping a reading there plays from where its
  own sentence was spoken, the same as tapping the closed block does.

  A paragraph nothing has been read of yet stays closed however the
  switch is set, there being nothing to compare it against, and its tag
  is a label with no chevron. The switch itself is not stored: it is
  how the transcript is being read just now, and it starts closed.
* A sentence whose reading hasn't arrived yet shows as it was said,
  faintly, in the language it was heard in, and gives way to its reading
  as that lands. The reading runs a sentence behind the speaking on
  purpose — a translator given half a sentence returns half a thought —
  and the words themselves say so better than a gap would. **Show what
  was said** (under *While a reading catches up*) turns that off, for a
  faint `…` in its place.
* Tapping a word to [play it back](#playing-back) and editing or cutting
  a selection work on what was said, which a translated paragraph is not
  showing. Tapping one of its sentences plays from where *that sentence*
  was spoken instead, which is as fine-grained as a translation can be:
  it has its own word count and its own word order, so the sentence is
  the smallest honest unit. Editing and cutting are for the paragraphs
  in your own language.

A paragraph in your own language is shown as it always was — it is
already in the language you read — as is one the translator gave up on,
where what was said is better than nothing.

Changing a language reloads the models, which takes a few seconds. A
recording in progress keeps the engine it started with, and the change
is applied the moment that recording ends.

## Voice typing keyboard

Scribe voice typing dictates into any app's text field.

1. In **⚙** → **Keyboard**, tap **Keyboard settings** and turn on **Scribe voice
   typing**.
2. In any app, open the keyboard and pick Scribe with the keyboard
   switcher.
3. It starts listening right away and shows the transcript as you talk.
   **Insert** types the finished text and switches back to your previous
   keyboard. **Cancel** discards it, and **⌨** switches keyboards
   without inserting.

The keyboard uses the models the app downloaded, so open Scribe once
first. It can't listen while the app is recording.

## Building from source

Needs Rust with the `aarch64-linux-android` target,
[cargo-ndk](https://github.com/bbqsrc/cargo-ndk), the Android NDK, the
Android SDK, a JDK 17 or newer and [just](https://github.com/casey/just).
The recipes default to Homebrew's locations on macOS. Set
`ANDROID_NDK_HOME`, `ANDROID_HOME` and `JAVA_HOME` to use others.

```bash
rustup target add aarch64-linux-android
cargo install cargo-ndk
just android-app          # debug APK, installed with adb if a device is connected
just android-apk          # release APK in dist/
```

The Android recipes build into `target/android`, their own Cargo target
dir: sherpa-onnx-sys caches its prebuilt libs per target dir, and the
Android ones would otherwise shadow the host's — its build script takes
the Android `jniLibs/` for a lib dir whatever the target is, so a later
host build goes looking for `.a` files among Android `.so` files.
`target/ios` is separate for the same reason.

`just android-apk` signs with the keystore in `SCRIBE_KEYSTORE` (plus
`SCRIBE_KEYSTORE_PASSWORD`, `SCRIBE_KEY_ALIAS` and `SCRIBE_KEY_PASSWORD`)
when it is set, and with the debug key otherwise. The release workflow
builds it on every version tag, using a keystore from the repository's
secrets when there is one (`SCRIBE_KEYSTORE_BASE64` and the three
passwords). `just android-keystore` creates that keystore in
`~/scribe.jks` and stores the secrets with `gh`. Each release must be
signed with the same key to install over the last, so if the secrets are
ever lost along with the keystore, the next release needs a fresh install.
`just android-app` signs with that same keystore when `~/scribe.jks`
exists, so local builds and releases install over each other. Its
password is `SCRIBE_KEYSTORE_PASSWORD` if set (for example in `.env`),
else `android`; you're asked only when that doesn't open it.

The app is in [android/](android/), Kotlin and Jetpack Compose over a
JNI bridge, [crates/vox_android](crates/vox_android), to the same
transcription engine as the desktop scribe.

### Translating

Every word the app says is in
`android/app/src/main/res/values/strings.xml`, with a comment over each
group saying where on the screen it appears. A language is that file
again under `values-<code>/`, where `<code>` is its ISO 639 code (`ru`,
`de`, `pt-rBR`), plus one line in
`android/app/src/main/res/xml/locales_config.xml` — without which
Android never offers the app that language on its own.

Three rules, all of them checked by `StringsTest` in `./gradlew test`,
which fails the build rather than letting a phone find out:

* **The same names.** A name in `values/` that the translation lacks is
  a crash on the screen that wanted it. One the translation has and
  `values/` does not is a line nobody will ever read.
* **The same arguments.** `%1$s` and `%2$d` must stay in the same places
  and keep their types; the sentence may put them wherever it reads
  best. Getting this wrong throws at the moment the sentence is said.
* **Enough plural forms.** English needs `one` and `other`. Russian
  needs `one`, `few`, `many` and `other`, because it says "1 абзац", "2
  абзаца" and "5 абзацев"; a missing form falls back to `other`
  silently. Add the language to `pluralForms` in `StringsTest` and the
  test will hold you to them.

Two habits make the rest of it easier. Keep a language's **name** in
whatever form reads both as a list item and inside a sentence — the
Russian file uses lower case ("русский") and rephrases the lines that
would need it declined, putting the name after a colon rather than in
the middle of a clause. And remember that Russian runs perhaps a third
longer than English: the dock's status line is one line and ellipsises,
so it is the place where a long translation is felt first.

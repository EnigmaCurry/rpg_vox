# Voice Room for Android

[The room](ROOM.md) as an app, so a call survives the screen going
off the way a phone's own calls do.

It is a wrapper and says so: the whole of the app is a `WebView` and a
foreground service. There is no audio code in it, because the room
already has all of it — what a browser tab cannot have, and what this
adds, is a foreground service of its own.

**It ships no address.** Whose room this is belongs to whoever installed
the app, so the first screen asks and the answer is kept. Paste the
invite link you were sent; after that the app remembers the room itself
rather than the link, because a link is spent by **Join Audio** and
keeping it would make every launch after the first open an expired page.
What gets you in from then on is the cookie.

The address has to be `https`. A browser gives the microphone only to a
secure page, so an `http` room is one you could read and never talk in —
better said at the door than found out as a microphone that will not
open. `localhost` is the exception browsers themselves make, and is how
this is tested against a room on a laptop (below).

**Back** goes back through the room, then out of the audio, then out of
the app. It never goes to that first screen: the box an address goes in
is for a phone the room does not know, and being asked to type what you
are already holding is no better than having nowhere to type it.

The way to point the app at a different room is to **log out** of the one
you are in, which is in the room's own settings and says what it costs. A
link you did not mean to follow is a special case and has its own way
out: the door of an unspent invite offers **This isn't my link**, which
puts it down without spending it.

While a call is up the notification holds it, and carries the two things
worth reaching without opening the app: **Mute**, which says `Mute` or
`Unmute` because it is the room's own button being pressed and the room's
own word being read back, and **Leave**, which leaves the audio and puts
you at the door. Neither logs you out. Leaving from there ends the
service with it, so there is no notification left offering to take you
back to a call nothing is in.

## Install

Every release carries `voice-room-<version>-android.apk`, built by the
release workflow and signed with the same key each time, so one installs
over the last. It is the same key scribe's APK is signed with; they are
different apps and Android only asks that an update be signed by
whatever signed the one before it.

### With Obtainium (recommended)

[Obtainium](https://github.com/ImranR98/Obtainium) installs apps
straight from their GitHub releases and updates them when a new one is
published.

1. Install Obtainium.
2. On the phone, open
   [Add Voice Room to Obtainium](https://apps.obtainium.imranr.dev/redirect?r=obtainium://add/https://github.com/EnigmaCurry/rpg_vox),
   or tap **Add App** in Obtainium and enter
   `https://github.com/EnigmaCurry/rpg_vox`.
3. In the app's settings, put this in **Custom APK link filter by
   regular expression**:

   ```
   voice-room-.*-android\.apk$
   ```

4. Tap **Add**, then **Install**. Android asks you to allow installs
   from Obtainium the first time.

The filter is not optional here. One repository publishes two apps, so a
release has two APKs in it, and without a filter Obtainium asks **Pick
an APK** on every release and the wrong answer installs
[scribe](SCRIBE_ANDROID.md) instead. Adding both apps to Obtainium is
adding the same repository twice, each with its own filter, which is
what tells them apart.

### By hand

Download `voice-room-<version>-android.apk` from the [releases
page](https://github.com/EnigmaCurry/rpg_vox/releases) on the phone and
open it. Android asks you to allow installs from your browser or file
manager the first time.

If a new version will not install over the old one, the two were signed
with different keys: uninstall Voice Room first. That loses the cookie,
so coming back takes a new invite link.

## Building

Needs an Android SDK; `local.properties` points at it, as in
[scribe's app](SCRIBE_ANDROID.md).

```bash
just room-apk          # release APK into dist/, as the workflow ships it
```

Or, for something to put on a phone over adb while working on it:

```bash
cd android-room && ./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

`just room-apk` signs with the keystore in `VOX_ROOM_KEYSTORE` (plus
`VOX_ROOM_KEYSTORE_PASSWORD`, `VOX_ROOM_KEY_ALIAS`,
`VOX_ROOM_KEY_PASSWORD`) when they are set, and with the debug key when
they are not. A debug key made on one machine is not the debug key made
on another, which is why an APK can refuse to install over its own
predecessor.

It is the same key [scribe's app](SCRIBE_ANDROID.md) is signed with, and
`just android-keystore` is what makes it. A key signs packages, not
apps, so one of them can sign both; the release workflow hands this job
the same secrets it hands that one, which are named `SCRIBE_*` because
scribe was the only Android app here when they were made.

There is no native code in it and nothing to cross-compile: it is Kotlin
and a WebView, so one APK covers every phone. That is the whole reason
it builds in a minute where [scribe's app](SCRIBE_ANDROID.md) builds in
twenty.

Android 11 (API 30) and up. Higher than scribe's 26 on purpose: a typed
foreground service wants 29, and reading the window insets without a
compatibility branch wants 30.

## Testing it against a room on your laptop

No certificate needed. `localhost` is a secure origin, and `adb reverse`
makes the laptop's port exactly that on the phone:

```bash
adb reverse tcp:7340 tcp:7340
just room --room Lobby --echo --no-stt --tts none
cargo run -p vox_room --release -- invite Pixel   # paste this into the app
```

`-e url` sets the address without typing it, which is what the screen-off
measurements below were driven with:

```bash
adb shell am start -n com.enigmacurry.vox_room/.MainActivity \
  -e url 'http://127.0.0.1:7340/lobby'
adb logcat -s VoxRoom:I
adb shell input keyevent 223                     # screen off
adb shell dumpsys display | grep mScreenState    # and it really is
```

## Why the odd parts are there

Four things cost an afternoon to find and none of them are discoverable
from the error they cause. They are why this app is not twenty lines.

**`MODIFY_AUDIO_SETTINGS`.** Without it *every* `getUserMedia` in a
WebView fails — including a bare `{ audio: true }` — with
`NotReadableError: Could not start audio source`. Nothing in that message
points at a permission. The only clue is in the log:

    audio_manager_android.cc: Unable to select communication device!

**The foreground service may not start before `RECORD_AUDIO` is
granted.** A microphone-typed service without it is a `SecurityException`
and the end of the process, so the service waits for the permission
result rather than starting in `onCreate`.

**`KeptVisibleWebView`.** Chromium releases a page's audio device about a
minute after the window it lives in stops being visible, and the screen
turning off is exactly that. Both AAudio streams go; the page keeps
running and the socket stays up, so the room goes on listing you while
hearing digital silence — the worst shape a failure can have, because
nothing anywhere reports an error. The view is what carries word that the
window went away, so the view is the only place an embedder can decline
to carry it. Measured, with it: the screen off and verified off at 0, 45,
90 and 135 seconds, and not one teardown, where without it the streams
closed at 63.

**A wake lock does not help**, and that is worth writing down so nobody
spends the afternoon again. It guarantees the CPU, and the CPU was never
what went missing: measured, the page's main thread was never throttled
at all — it answered in 2–6 ms and its clock advanced ~1002 ms a second,
continuously, *including across the moment the audio died*.

**The insets go on a frame around the WebView, not on the WebView.**
Everything targeting Android 15 is laid out edge to edge whether it asked
to be or not, and padding a WebView does not move the page: it goes on
being laid out against the full height of the view, and so on drawing
under the clock — which is where the room puts the heading you change
rooms from, so the thing you could not reach was the thing you most
needed. Making the view itself smaller is what the page notices.

## What is still unknown

Whether the phone can still *hear* with the screen off. Only the
direction out of it was ever measured, because only that one can be read
from the server: the mixer writes a level for every person ten times a
second, and an input that has run dry is padded with exactly zero where a
live microphone never is. A headset settles the other direction in a
minute.

The room's own interface is a desktop one. It works here and it is not
yet *designed* here; see the note above about the heading under the
clock, which was the first of those and will not be the last.

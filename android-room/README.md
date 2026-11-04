# Voice Room for Android

[The room](../ROOM.md) as an app, so a call survives the screen going
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

**Back** goes back through the room; at the door it comes out to that
first screen again, which is also how you point the app at a different
room. **Leave** in the notification does the same and ends the call.

## Building

Needs an Android SDK; `local.properties` points at it, as in
[scribe's app](../ANDROID.md).

```bash
cd android-room && ./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

For something installable over a previous release, set `VOX_ROOM_KEYSTORE`
(and `VOX_ROOM_KEYSTORE_PASSWORD`, `VOX_ROOM_KEY_ALIAS`,
`VOX_ROOM_KEY_PASSWORD`) and `./gradlew assembleRelease`. Without a key
the debug key signs, and a debug key built somewhere else is a different
key, which is why an APK can refuse to install over its own predecessor.

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

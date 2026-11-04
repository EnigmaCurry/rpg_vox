# Voice Room in a WebView: does a call survive the screen going off?

A spike, not an app. It is a `WebView` pointed at a voice room and a
foreground service holding the process up, and it exists to answer one
question that nothing in the web app could answer on its own.

It adds no audio code. Everything the room needs is already in
`crates/vox_room/web`; what a browser tab cannot have, and what this
adds, is a foreground service of its own.

**The answer is yes, with two things you would not guess.**

## What was measured

A Pixel 6 on GrapheneOS, Android 17 (API 37), Vanadium WebView 156,
against a room running on the laptop. Two numbers, from opposite ends:

* **Is the page still being scheduled?** The app asks the page what time
  it thinks it is, once a second, and logs how late the answer was. This
  is the thing the audio loop was moved off the main thread to survive
  (see `web/src/lib/audio.worker.js`), so it is reported separately.
* **Is the audio still arriving?** Not measured here at all. The server
  already knows: the mixer writes a level for every person in the room
  ten times a second, and an input that has run dry is padded with
  *exactly* zero, where a live microphone in a quiet room never is. So
  somebody watching on a plain socket sees this phone go silent the
  moment its frames stop. Keeping the measurement outside the thing
  being measured is the point.

## What it found

**The page's main thread is never throttled.** Held by a
microphone-typed foreground service, with the screen off, the page
answered in 2–6 ms and its clock advanced ~1002 ms per second,
continuously — including across the moment the audio died. The worry
that moved the audio loop into a worker does not materialise here. The
worker is still right; this is simply not where Android bites.

**Chromium releases the audio device about a minute after the WebView's
window stops being visible**, which is what the screen turning off
amounts to. Both AAudio streams go, capture and playback. The page keeps
running and the socket stays up, so the room goes on listing the person
while receiving digital silence from them. Twice measured: 63 s and
~76 s.

**A wake lock does not help.** It guarantees the CPU, and the CPU was
never the problem. Measured: identical teardown, same minute.

**Swallowing the news does.** The view is what carries word that the
window went away, so the view is the only place an embedder can decline
to carry it — see `KeptVisibleWebView`. With it, four minutes of
screen-off, no wake lock, not one silent second.

## Two permissions, one of them not obvious

* `RECORD_AUDIO`, and the foreground service must not be started until it
  has actually been granted: a microphone-typed service without it is a
  `SecurityException` and the end of the process on Android 14+.
* `MODIFY_AUDIO_SETTINGS`, without which **every** `getUserMedia` fails —
  including a bare `{ audio: true }` — with `NotReadableError: Could not
  start audio source`. Nothing in that message points at it. The only
  clue is in the log:

      audio_manager_android.cc: Unable to select communication device!

## Running it

The room must be `https`, because a browser only gives the microphone to
a secure page. For testing there is a way around that which needs no
certificate: `localhost` is a secure origin, and `adb reverse` makes the
laptop's room exactly that.

```bash
adb reverse tcp:7340 tcp:7340          # the phone's localhost is the laptop's
(cd android-room && ./gradlew assembleDebug)
adb install -r android-room/app/build/outputs/apk/debug/app-debug.apk

# By hand: the app asks for a URL. Or drive it:
adb shell am start -n com.enigmacurry.vox_room_spike/.MainActivity \
  -e url 'http://127.0.0.1:7340/lobby' --ez wakelock false --ez autojoin true

adb logcat -s VoxSpike:I                # the heartbeat, and the page's console
adb shell input keyevent 223            # screen off
adb shell dumpsys display | grep mScreenState   # and it really is
```

`--ez autojoin true` ticks the consent box and presses Join Audio from
JavaScript, so a run needs no hands. `--ez wakelock true` holds a
`PARTIAL_WAKE_LOCK`, which is there to be measured rather than because
it is wanted.

## What is still unknown

Whether the phone can still *hear*. Only the direction out of the phone
was measured, because only that one can be read from the server. That
the playback stream is no longer torn down is good evidence and not
proof; a headset settles it in a minute.

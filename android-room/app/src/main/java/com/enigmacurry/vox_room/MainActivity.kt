package com.enigmacurry.vox_room

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.graphics.Color
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.util.TypedValue
import android.view.Gravity
import android.view.KeyEvent
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher
import android.webkit.CookieManager
import android.webkit.PermissionRequest
import android.webkit.WebChromeClient
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Button
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import java.net.HttpURLConnection
import java.net.URL
import kotlin.math.PI
import kotlin.math.min
import kotlin.math.sin
import org.json.JSONObject
import org.json.JSONTokener

/**
 * A voice room in a WebView, held up by a foreground service.
 *
 * The app ships no address. Whose room this is belongs to whoever
 * installed it, so the first screen asks, and the answer is kept. That
 * is the whole of the native UI; everything after it is the room's own.
 */
class MainActivity : Activity() {
    private var web: WebView? = null
    private val prefs by lazy { getSharedPreferences(PREFS, MODE_PRIVATE) }

    /// The notification's buttons have to reach the WebView, which the
    /// service cannot touch. Both of them are presses on the room's own
    /// controls -- the room is a page, and that is how a page is worked.
    private val fromNotification = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            when (intent?.action) {
                RoomService.ACTION_LEAVE -> leaveAudio()
                RoomService.ACTION_MUTE -> toggleMute()
            }
        }
    }

    /// Leave the audio, and let the service go with it.
    ///
    /// Not a log out, and not a way back to the box the address goes in.
    /// It used to be both: it threw the page away and showed that box,
    /// which from the outside is indistinguishable from being logged
    /// out, and logging out is not a thing a stray tap on a notification
    /// should be able to do. The room is still this phone's room
    /// afterwards; the door is simply where you are when you are not in
    /// the audio.
    private fun leaveAudio() {
        val w = web
        if (w == null) return RoomService.stop(this)
        w.evaluateJavascript(LEAVE_JS) {
            RoomService.stop(this)
        }
    }

    /// Mute or unmute, by pressing the room's own button.
    private fun toggleMute() {
        val w = web ?: return
        w.evaluateJavascript(MUTE_JS) {
            // The label follows the press a frame later.
            w.postDelayed({ askTheRoom() }, 300)
        }
    }

    /// Ask the room what it is doing with your microphone.
    ///
    /// Two answers come back from the one question, because both are
    /// read off the same button: what the notification should offer, and
    /// whether the phone's talk key is the microphone's right now (see
    /// [dispatchKeyEvent]).
    ///
    /// Watched rather than written when we happen to press something.
    /// The mute in the footer is the same mute, and somebody using it
    /// leaves the notification offering the opposite of the truth --
    /// which was the first thing anybody noticed about it. There is no
    /// event to listen for from out here, so it is asked, once a second,
    /// for as long as there is a room on the screen.
    ///
    /// The cost is a few microseconds of a WebView that is already
    /// running: the screen-off measurements in README.md were taken with
    /// exactly this, a `Date.now()` asked at the same rate, answering in
    /// 2 to 6 ms without once falling behind.
    private val watching = Handler(Looper.getMainLooper())
    private var lastSaid: String? = null

    private val watchTheTalkButton = object : Runnable {
        override fun run() {
            askTheRoom()
            watching.postDelayed(this, 1000)
        }
    }

    private fun askTheRoom() {
        val w = web ?: return forget()
        w.evaluateJavascript(STATE_JS) { answer ->
            val said = runCatching {
                JSONObject(JSONTokener(answer).nextValue() as String)
            }.getOrNull()
            pttNow = said?.optBoolean("ptt") ?: false
            typingNow = said?.optBoolean("typing") ?: false
            roomVolume = said?.optString("volume").orEmpty()
            roomChirp = said?.optString("chirp").orEmpty()
            say(said?.optString("says").orEmpty())
        }
    }

    /// No room on the screen: nothing to mute, and no key to lend.
    private fun forget() {
        pttNow = false
        typingNow = false
        // Not the room's answers. They are about this person's phone and
        // not about whether a page happens to be up, and throwing them
        // away here would mean the key reverted for as long as it took
        // the next room to load.
        say("")
    }

    // -- the phone's own talk key ------------------------------------

    /// Whether the room is in push to talk, and whether the focus is in
    /// something with a cursor in it. Both off the poll above, because
    /// the decision below has to be made without waiting: `evaluateJavascript`
    /// answers later and a key event has to be claimed or let go of now.
    ///
    /// The cost is that each is up to a second stale. What that buys is
    /// worth more than it costs in either direction: a second after
    /// turning push to talk on, the key talks, and a second after
    /// touching the message box it goes back to being a volume key.
    private var pttNow = false
    private var typingNow = false

    /// What the room says this person's phone should do, or empty for
    /// nothing said. Off the same poll, for the same reason.
    private var roomVolume = ""
    private var roomChirp = ""

    /// Whether the volume keys talk: the room's answer when there is
    /// one, and this phone's own when there is not.
    ///
    /// The room wins rather than merely filling a gap, because the point
    /// of putting it there was that it should reach a phone that has
    /// already been set up. A phone that quietly kept its own answer
    /// would be the one phone the setting could not fix. What keeps that
    /// from being a trap is that the app's own chooser writes *through*
    /// to the room when it can -- see [TalkKeyActivity] -- so changing
    /// it here changes the thing that wins.
    private fun whichWay(): Boolean =
        if (roomVolume.isNotEmpty()) TalkKey.on(roomVolume) else TalkKey.volume(prefs)

    private fun chirps(): Boolean =
        if (roomChirp.isNotEmpty()) TalkKey.on(roomChirp) else TalkKey.chirp(prefs)

    /// How long a volume key has to be down before it is the
    /// microphone's rather than the phone's.
    ///
    /// Everything about the feel of this is this number. Too short and
    /// a press meant for the volume talks; too long and the wait before
    /// a sentence is a wait you notice. 250 ms is about the floor at
    /// which a deliberate tap still reads as a tap, and it is one
    /// constant so it can be tried at 180 and at 350.
    private val holdMs = 250L

    /// The key that is down but has not yet been down long enough, or
    /// zero. Between this and [holding] is the only window in which a
    /// press is still both things it could be.
    private var waiting = 0

    /// The key code that opened the microphone, or zero. Remembered
    /// rather than re-derived, so that whatever changes while a key is
    /// held -- the setting, push to talk, the focus -- the key that
    /// opened the microphone is still the key that shuts it. A
    /// microphone left open because the answer moved underneath the hold
    /// would be the worst bug this app could have.
    private var holding = 0

    /// Lend a volume key to the microphone.
    ///
    /// Taken here rather than in `onKeyDown` so the question is asked
    /// before the view hierarchy sees it: the volume keys never reach
    /// the page as key events anyway, and claiming one this early is
    /// also what stops Android's own volume handling further down.
    ///
    /// Only while there is a room on the screen and push to talk is on
    /// in it -- a volume key that had stopped changing the volume
    /// whenever this app was open would be a bad trade for a feature
    /// that is off most of the time. And only outside a text box, which
    /// is the same stand-down the room's own talk key makes: with the
    /// cursor in the message field the key is the field's, and here that
    /// means it is the phone's.
    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        val code = event.keyCode
        when (event.action) {
            KeyEvent.ACTION_DOWN -> {
                // Held, not pressed again. Android repeats a held key
                // and the room counts one hold as one thing said.
                //
                // The `!= 0` tests are not redundant: an unmapped key is
                // `KEYCODE_UNKNOWN`, which is zero, and without them a
                // stray one of those would be swallowed by a hold that
                // is not happening.
                if (code != 0 && (code == holding || code == waiting)) return true
                if (!lentToTheMic(code)) return super.dispatchKeyEvent(event)
                waiting = code
                // The chirp fills the wait rather than following it: it
                // ends as the microphone opens, so what it teaches is
                // exactly the thing worth teaching, and the microphone
                // it is about never hears it.
                chirp()
                watching.postDelayed(becomeAHold, holdMs)
                return true
            }
            KeyEvent.ACTION_UP -> {
                // Let go before it was a hold: a tap, which is the
                // phone's. The volume change is made rather than handed
                // back -- an event already claimed cannot be returned,
                // and re-injecting one wants a permission only the
                // platform's own apps are given.
                if (code != 0 && code == waiting) {
                    waiting = 0
                    watching.removeCallbacks(becomeAHold)
                    hush()
                    turnTheVolume(code)
                    return true
                }
                if (code == 0 || code != holding) return super.dispatchKeyEvent(event)
                holding = 0
                hold(false)
                return true
            }
        }
        return super.dispatchKeyEvent(event)
    }

    /// Down long enough: the key is the microphone's now.
    private val becomeAHold = Runnable {
        val code = waiting
        if (code == 0) return@Runnable
        waiting = 0
        holding = code
        hold(true)
    }

    /// A tap, done ourselves, with the system left to say which stream
    /// a volume press in a call is even about -- which is a question
    /// worth not answering here, since the room holds a communication
    /// device and the honest answer changes with it.
    private fun turnTheVolume(code: Int) {
        val up = code == KeyEvent.KEYCODE_VOLUME_UP
        getSystemService(AudioManager::class.java).adjustSuggestedStreamVolume(
            if (up) AudioManager.ADJUST_RAISE else AudioManager.ADJUST_LOWER,
            AudioManager.USE_DEFAULT_STREAM_TYPE,
            AudioManager.FLAG_SHOW_UI,
        )
    }

    /// Either volume key, never one of them. A tap still changes the
    /// volume -- see the `ACTION_UP` arm above -- so lending both costs
    /// nothing that has to be weighed up, and which one it is was a
    /// question that only ever made somebody stop and think.
    private fun lentToTheMic(code: Int): Boolean {
        if (web == null || !pttNow || typingNow) return false
        if (code != KeyEvent.KEYCODE_VOLUME_UP && code != KeyEvent.KEYCODE_VOLUME_DOWN) return false
        return whichWay()
    }

    /// Press the room's talk key, from out here.
    ///
    /// The key and not the button: the room's own `onkeydown` is where
    /// push to talk, the toggle, a mute that came from the room's mixer
    /// and the text-box stand-down are all already decided, and a
    /// synthetic press through it inherits every one of them. Pressing
    /// the on-screen button instead would mean a `pointerdown` carrying
    /// a pointer id that does not exist, which `setPointerCapture`
    /// throws on before the room ever hears about it.
    private fun hold(down: Boolean) {
        // Said out loud, like the talk button's label is: a volume key
        // that has stopped talking looks exactly like a volume key that
        // was never lent, and this is the line that tells the two apart.
        Log.i(RoomService.TAG, "the talk key is ${if (down) "down" else "up"}")
        web?.evaluateJavascript(talkJs(down), null)
    }

    /// Let go, whatever is going on. Called when this leaves the front:
    /// a key cannot be held through that, and a microphone that thinks
    /// it still is would stay open.
    private fun letGo() {
        watching.removeCallbacks(becomeAHold)
        waiting = 0
        hush()
        if (holding == 0) return
        holding = 0
        hold(false)
        // No chirp. One says the microphone is open and the other says
        // it has closed, and both are for somebody listening; this is
        // the app going away under them, where a noise is just a noise.
    }

    // -- the chirp ----------------------------------------------------

    /// Two beeps rising, exactly as long as the wait they fill.
    ///
    /// Only the rising one. It is the whole reason the wait is bearable
    /// -- a hold that does nothing for a quarter of a second is a hold
    /// you talk over -- and this app is the only thing that can play it,
    /// because nothing is said to the page until the wait is over and a
    /// hold has been told from a tap.
    ///
    /// The falling pair that says the microphone has closed is the
    /// room's, and is played by the page like every other way of
    /// letting go of the key. There is no wait on that edge and so
    /// nothing only this app can do, and when both of them played it
    /// you heard it twice. See `lib/chirp.js`.
    private var rise: AudioTrack? = null

    private fun chirp() {
        if (!chirps()) return
        val track = rise ?: beeps().also { rise = it }
        runCatching {
            // How a static track is played a second time.
            track.stop()
            track.reloadStaticData()
            track.play()
        }.onFailure {
            // Said rather than swallowed. A chirp that does not play is
            // silent either way, and the first time this went wrong the
            // only evidence was the silence.
            Log.w(RoomService.TAG, "the chirp did not play: $it")
        }
    }

    /// Cut a chirp short: the press turned out to be a tap, and the
    /// rest of a sound that means `you may speak` is a lie.
    private fun hush() {
        runCatching { rise?.stop() }
    }

    /// The chirp runs through the wait and stops as the microphone
    /// opens, which puts it in the one window where the microphone
    /// cannot hear it -- on a speakerphone a chirp played *after* the
    /// open goes out to the room with the first word of every sentence.
    ///
    /// The price is that a press which turns out to be a tap has
    /// already started one. So it does not start on the press: the
    /// first [lead] of the sound is silence, and only a tap held longer
    /// than that is audible at all. A quarter of a second is a hold; a
    /// tap is nothing like 120 ms; the overlap is small and what lands
    /// in it is a fragment of a beep rather than a whole one.
    private fun beeps(): AudioTrack {
        val rate = 44100
        val lead = 120                       // ms of silence first: the
                                             // room a tap gets to be a
                                             // tap in, silently
        val beep = 55                        // ms of each
        val gap = 20                         // ms of silence between
        val low = 700.0
        val high = 1050.0                    // a fifth above, which is
                                             // a pleasant pair and high
                                             // enough for a phone to
                                             // actually produce
        val pcm = ShortArray(rate * (lead + beep * 2 + gap) / 1000)
        val len = rate * beep / 1000
        val fade = rate * 0.006              // 6 ms on and off each beep:
                                             // a sine that starts at full
                                             // height is a click, and a
                                             // click in a headset is worse
                                             // than no chirp at all
        fun tone(at: Int, freq: Double) {
            for (i in 0 until len) {
                val shape = min(1.0, min(i, len - i) / fade)
                val s = sin(2.0 * PI * freq * i / rate) * shape * 0.35
                pcm[at + i] = (s * Short.MAX_VALUE).toInt().toShort()
            }
        }
        tone(rate * lead / 1000, low)
        tone(rate * (lead + beep + gap) / 1000, high)

        val track = AudioTrack.Builder()
            // Where the room is, which is the only place this is any
            // use. Signalling was the obvious choice and was wrong:
            // `USAGE_VOICE_COMMUNICATION_SIGNALLING` is the DTMF stream,
            // which on a phone that is not in a telephony call is routed
            // somewhere nobody can hear. The room plays on music -- a
            // volume key pressed in a room moves the music stream, which
            // is how this was settled -- so the chirp plays there too,
            // at whatever the room is already set to.
            .setAudioAttributes(AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                .build())
            .setAudioFormat(AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(rate)
                .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                .build())
            .setBufferSizeInBytes(pcm.size * 2)
            .setTransferMode(AudioTrack.MODE_STATIC)
            .build()
        track.write(pcm, 0, pcm.size)
        return track
    }

    /// Only when it has actually changed: each of these is a service
    /// start and a notification posted again.
    private fun say(said: String) {
        if (said == lastSaid) return
        lastSaid = said
        Log.i(RoomService.TAG, "the talk button says: ${said.ifEmpty { "nothing to mute" }}")
        RoomService.says(this, said)
    }

    override fun onCreate(saved: Bundle?) {
        super.onCreate(saved)
        registerReceiver(
            fromNotification,
            IntentFilter(RoomService.ACTION_LEAVE).apply { addAction(RoomService.ACTION_MUTE) },
            RECEIVER_NOT_EXPORTED,
        )
        val want = mutableListOf(Manifest.permission.RECORD_AUDIO,
            Manifest.permission.POST_NOTIFICATIONS)
        val missing = want.filter { checkSelfPermission(it) != PackageManager.PERMISSION_GRANTED }
        if (missing.isNotEmpty()) {
            // Nothing may start until this comes back: a microphone-typed
            // foreground service with no RECORD_AUDIO is a SecurityException
            // and the end of the process, and a WebView that asks for the
            // mic while the dialog is up is told the device won't open.
            requestPermissions(missing.toTypedArray(), 1)
            return
        }
        begin()
    }

    override fun onRequestPermissionsResult(
        code: Int, perms: Array<out String>, granted: IntArray,
    ) {
        super.onRequestPermissionsResult(code, perms, granted)
        val mic = perms.indexOf(Manifest.permission.RECORD_AUDIO)
        if (mic >= 0 && granted.getOrNull(mic) != PackageManager.PERMISSION_GRANTED) {
            setup(getString(R.string.no_mic))
            return
        }
        begin()
    }

    /// Where to go back to: the room's address, kept so a phone that is
    /// already known goes straight there.
    private val roomUrl get() = prefs.getString("url", "").orEmpty()

    /// What was last typed in and has not yet got anybody in. Kept
    /// separately from the address because the two have different
    /// lives: an invite link is spent the moment it works, and a box
    /// offering a spent link back is offering nothing. Cleared when the
    /// room says this phone is in, which is the moment it was consumed.
    private val typedUrl get() = prefs.getString("typed", "").orEmpty()

    private fun consumeTyped() {
        if (typedUrl.isNotEmpty()) prefs.edit().remove("typed").apply()
    }

    private fun begin() {
        // `-e url` lets a run of this be scripted; see README.md.
        val given = intent?.getStringExtra("url")?.trim()
        // A cookie for the room is this phone being known to it, and
        // the address is then the thing to open: the link that got us
        // there is spent, and asking the room about a spent link is a
        // roundabout way of finding out what the cookie already says.
        // With no cookie, a link typed in but not yet used is the only
        // thing worth anything, and the bare address is not.
        val known = roomUrl.isNotEmpty() &&
            !CookieManager.getInstance().getCookie(roomUrl).isNullOrEmpty()
        val where = given?.takeIf { it.isNotEmpty() }
            ?: roomUrl.takeIf { known }
            ?: typedUrl.takeIf { it.isNotEmpty() }
            ?: roomUrl
        if (where.isEmpty()) return setup()
        // Ask before showing anything. The room's own page for somebody
        // it does not know says to go and ask for an invite link, and
        // loading it first only to leave it again puts that page on the
        // screen for a moment on the way to the one that can actually
        // take a link. Known: the door. Not known: the field.
        checking()
        askDoor(where) { state ->
            when (state) {
                // In already. Whatever got us here is spent -- the room
                // says `in` for a link of your own you have used before
                // -- so the box stops offering it back, and we go to the
                // room's address rather than through the spent link.
                "in" -> { consumeTyped(); enter(roomUrl.ifEmpty { where }) }
                "invited" -> enter(where)
                "expired" -> { consumeTyped(); setup(getString(R.string.link_spent)) }
                "none" -> setup(getString(R.string.need_link))
                else -> setup(getString(R.string.setup_unreachable))
            }
        }
    }

    /// While the room is being asked. Neither answer's screen, because
    /// either one shown now would be a guess, and the wrong guess is the
    /// one that looks like being thrown out.
    private fun checking() {
        letGo()
        watchBack(false)
        web?.destroy()
        web = null
        val dp = { n: Int -> TypedValue.applyDimension(
            TypedValue.COMPLEX_UNIT_DIP, n.toFloat(), resources.displayMetrics).toInt() }
        val col = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(24), dp(24), dp(24), dp(24))
            addView(TextView(context).apply {
                text = getString(R.string.setup_title)
                setTextSize(TypedValue.COMPLEX_UNIT_SP, 28f)
                setPadding(0, 0, 0, dp(8))
            })
            addView(TextView(context).apply {
                text = getString(R.string.setup_checking)
                setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            })
        }
        setContentView(col)
        insetBySystemBars(col)
    }

    /// A second launch is an intent into the instance already here.
    override fun onNewIntent(next: Intent) {
        super.onNewIntent(next)
        intent = next
        // Only an intent that actually asks for something starts the
        // room over. Coming back from the notification is also an intent
        // into this instance, and reloading the page for it would drop
        // the call the notification is about.
        val given = next.getStringExtra("url")?.trim()
        val asked = !given.isNullOrEmpty()
        if (!asked) return
        letGo()
        web?.destroy()
        web = null
        // Through the same decision as a fresh launch, so an intent
        // cannot walk past the question of whether there is a way in.
        begin()
    }

    // -- the one screen this app has of its own ------------------------

    /// Ask where the room is. Shown on the first run, when the room is
    /// left from the notification, and whenever Back reaches the door.
    private fun setup(problem: String? = null) {
        RoomService.stop(this)
        letGo()
        watchBack(false)
        watching.removeCallbacks(watchTheTalkButton)
        web?.destroy()
        web = null

        val dp = { n: Int -> TypedValue.applyDimension(
            TypedValue.COMPLEX_UNIT_DIP, n.toFloat(), resources.displayMetrics).toInt() }
        val title = TextView(this).apply {
            text = getString(R.string.setup_title)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 28f)
            setPadding(0, 0, 0, dp(8))
        }
        val blurb = TextView(this).apply {
            text = getString(R.string.setup_blurb)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            setPadding(0, 0, 0, dp(20))
        }
        val field = EditText(this).apply {
            hint = getString(R.string.setup_hint)
            setText(typedUrl)
            inputType = android.text.InputType.TYPE_CLASS_TEXT or
                android.text.InputType.TYPE_TEXT_VARIATION_URI
            setSingleLine()
        }
        val warn = TextView(this).apply {
            setTextColor(Color.parseColor("#FF8A80"))
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
            setPadding(0, dp(12), 0, 0)
            visibility = View.GONE
        }
        val go = Button(this).apply {
            text = getString(R.string.setup_go)
            val lp = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            lp.topMargin = dp(20)
            layoutParams = lp
        }
        if (problem != null) {
            warn.text = problem
            warn.visibility = View.VISIBLE
        }
        field.addTextChangedListener(object : android.text.TextWatcher {
            override fun afterTextChanged(s: android.text.Editable?) {
                // A complaint about the last address should not outlive it.
                warn.visibility = View.GONE
            }
            override fun beforeTextChanged(c: CharSequence?, a: Int, b: Int, d: Int) {}
            override fun onTextChanged(c: CharSequence?, a: Int, b: Int, d: Int) {}
        })

        val col = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(24), dp(24), dp(24), dp(24))
            addView(title); addView(blurb); addView(field); addView(warn); addView(go)
        }
        val page = ScrollView(this).apply {
            addView(col, ViewGroup.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        }
        setContentView(page)
        insetBySystemBars(page)

        go.setOnClickListener {
            when (val said = check(field.text.toString().trim())) {
                is Where.Bad -> { warn.text = said.why; warn.visibility = View.VISIBLE }
                is Where.Good -> {
                    prefs.edit().putString("typed", said.url).apply()
                    enter(said.url)
                }
            }
        }
    }

    private sealed class Where {
        class Good(val url: String) : Where()
        class Bad(val why: String) : Where()
    }

    /// What the field will accept. `https` or nothing: a browser hands
    /// over the microphone only to a secure page, so an `http` room is
    /// one you could read and never talk in -- better said here than
    /// found out as a microphone that will not open. `localhost` is the
    /// exception the browsers themselves make, and is how this is tested
    /// against a room on a laptop (see README.md).
    private fun check(said: String): Where {
        if (said.isEmpty()) return Where.Bad(getString(R.string.setup_need_url))
        val with = if ("://" in said) said else "https://$said"
        val uri = runCatching { Uri.parse(with) }.getOrNull()
        val host = uri?.host
        if (uri == null || host.isNullOrEmpty()) return Where.Bad(getString(R.string.setup_not_a_url))
        val local = host == "localhost" || host == "127.0.0.1"
        if (uri.scheme != "https" && !local) return Where.Bad(getString(R.string.setup_need_https))
        return Where.Good(with)
    }

    // -- the room ------------------------------------------------------

    @SuppressLint("SetJavaScriptEnabled")
    private fun enter(url: String) {
        RoomService.start(this)
        CookieManager.getInstance().setAcceptCookie(true)
        val w = KeptVisibleWebView(this)
        web = w
        w.settings.apply {
            javaScriptEnabled = true
            // The theme is read out of localStorage before anything is
            // drawn, and the invite is spent for a cookie.
            domStorageEnabled = true
            // The room starts its AudioContext from the Join Audio click,
            // and that click is the page's own rather than a gesture this
            // WebView will vouch for. Without this the context stays
            // suspended and the room is silent.
            mediaPlaybackRequiresUserGesture = false
        }
        w.webChromeClient = object : WebChromeClient() {
            override fun onPermissionRequest(request: PermissionRequest) {
                // The page asking for the microphone is the whole reason
                // the service is microphone-typed. Android has already
                // asked the person; this is the page asking the app.
                val mic = request.resources.filter {
                    it == PermissionRequest.RESOURCE_AUDIO_CAPTURE
                }
                if (mic.isEmpty()) return request.deny()
                runOnUiThread { request.grant(mic.toTypedArray()) }
            }
        }
        w.webViewClient = object : WebViewClient() {
            override fun onPageFinished(view: WebView, finished: String) {
                // A page with no way forward sends them to the one screen
                // that has somewhere to put a link.
                wayIn(finished)
                // Remember where the room is, not how we got in. An invite
                // link is spent by Join Audio, so keeping the one that was
                // pasted would make every launch after the first open an
                // expired page. The room rewrites the address once the
                // link is spent, and what is kept is that address's root:
                // the cookie is what gets us in from then on.
                val uri = runCatching { Uri.parse(view.url) }.getOrNull()
                if (uri?.scheme != null && uri.authority != null) {
                    prefs.edit().putString("url", "${uri.scheme}://${uri.authority}").apply()
                }
            }
        }
        // The insets go on a frame around the WebView rather than on the
        // WebView: padding a WebView does not move the page, which goes
        // on being laid out against the full width and height of the
        // view and so on drawing under the clock. Making the view itself
        // smaller is what the page notices.
        val frame = FrameLayout(this).apply {
            addView(w, FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        }
        setContentView(frame, ViewGroup.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        insetBySystemBars(frame)
        watchBack(true)
        lastSaid = null
        watching.removeCallbacks(watchTheTalkButton)
        watching.post(watchTheTalkButton)
        w.loadUrl(url)
    }

    /// Keep a view out from under the status bar and the gesture bar.
    ///
    /// Everything targeting Android 15 is laid out edge to edge whether
    /// it asked to be or not, and a WebView that fills the window draws
    /// the page under the clock -- which is where the room puts the
    /// heading you change rooms from, so the thing you could not reach
    /// was the thing you most needed. The page is given the room that is
    /// actually free instead.
    private fun insetBySystemBars(v: View) {
        v.setOnApplyWindowInsetsListener { view, insets ->
            val bars = insets.getInsets(
                WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            insets
        }
        v.requestApplyInsets()
    }

    /// Back goes back through the room; at the door it comes out to the
    /// app's own screen, which is how you point this at a different room.
    ///
    /// Registered rather than overridden. `onBackPressed` is not called
    /// at all for anything targeting Android 15 or later -- predictive
    /// back is on by default there and the system dispatches through
    /// `OnBackInvokedDispatcher` instead -- so the override that used to
    /// be here was dead code, and back simply left the app.
    ///
    /// It is registered only while the room is up. On the app's own
    /// screen back means leave, which is the system's job and not ours.
    private val goingBack = if (Build.VERSION.SDK_INT >= 33) {
        OnBackInvokedCallback { back() }
    } else {
        null
    }
    private var backRegistered = false

    /// What the room last said this phone was to it. Back reads it: the
    /// door of a room you are logged into is the end of the road and
    /// back means leave, but the door of a link you are only holding
    /// has the app's own screen behind it.
    private var lastWayIn: String? = null

    private fun watchBack(on: Boolean) {
        val cb = goingBack ?: return
        if (on == backRegistered) return
        if (on) {
            onBackInvokedDispatcher.registerOnBackInvokedCallback(
                OnBackInvokedDispatcher.PRIORITY_DEFAULT, cb)
        } else {
            onBackInvokedDispatcher.unregisterOnBackInvokedCallback(cb)
        }
        backRegistered = on
    }

    /// Back through the room, then out of the audio, then out of the app.
    ///
    /// Never to the box the address goes in. That screen is for a phone
    /// the room does not know, and backing into it from inside a room
    /// you are logged into asks you to type what you are already
    /// holding. The way to point this at a different room is to log out
    /// of the one you are in, which the room's own settings do, and
    /// which lands here by way of [wayIn].
    ///
    /// The room's own door is the step between: whether there is one to
    /// go to is the same question as whether the audio is joined, and
    /// the room answers it by having a Leave audio button or not having
    /// one. Pressing its button rather than loading the door ourselves,
    /// so the sockets are let go of the way the room lets go of them.
    private fun back() {
        val w = web ?: return
        if (w.canGoBack()) return w.goBack()
        // One layer at a time. The room has pages and panels over itself
        // -- settings, the owner's page, somebody's desktop, a profile,
        // a dropdown -- and every one of them is a thing Escape closes
        // rather than a page the WebView navigated to, so `canGoBack` is
        // false inside all of them and back used to mean leave. Backing
        // out of the settings page put the whole app away.
        w.evaluateJavascript(CLOSE_JS) { shut ->
            if (shut != null && shut.contains("closed")) return@evaluateJavascript
            leaveOrFinish(w)
        }
    }

    /// Nothing left open: back through the audio, and then out.
    private fun leaveOrFinish(w: WebView) {
        w.evaluateJavascript(
            """(() => {
                 const leave = document.querySelector('.tools button[aria-label="Leave audio"]');
                 if (!leave) return 'door';
                 leave.click();
                 return 'left';
               })()"""
        ) { answer ->
            // Already at the door: there is nowhere further back inside
            // the room, so back means what it means everywhere else.
            if (answer != null && !answer.contains("door")) return@evaluateJavascript
            // At the door. Logged in, there is nothing behind it; only
            // holding a link, the screen that takes one is.
            if (lastWayIn == "in") finish() else setup()
        }
    }

    /// Android 11 and 12 still come through here.
    @Suppress("OverrideDeprecatedMigration", "DEPRECATION")
    override fun onBackPressed() {
        if (web != null) back() else super.onBackPressed()
    }

    /// Ask the room whether this phone can get in at all.
    ///
    /// After **Log out**, and after a link that has already been spent,
    /// the page is a dead end: it says to ask whoever runs the room for
    /// an invite, and there is nowhere on it to put one. The place a
    /// link goes is the app's own screen, so that is where this sends
    /// them -- otherwise the only way to use a different link is to
    /// clear the app's data.
    ///
    /// Asked of the room rather than read off the page: the page says it
    /// in whichever language it is wearing, and this has to be true in
    /// all of them.
    private fun wayIn(pageUrl: String?) {
        askDoor(pageUrl) { state ->
            if (state == "in") consumeTyped()
            val why = when (state) {
                "none" -> getString(R.string.need_link)
                "expired" -> { consumeTyped(); getString(R.string.link_spent) }
                // Anything else, including not being able to ask, leaves
                // a page that is already up alone: it is working until
                // something says otherwise.
                else -> return@askDoor
            }
            if (web != null) setup(why)
        }
    }

    /// Ask the room what this phone is to it, and say so on the main
    /// thread. `null` means the room could not be asked at all.
    private fun askDoor(pageUrl: String?, then: (String?) -> Unit) {
        val uri = runCatching { Uri.parse(pageUrl) }.getOrNull()
        val scheme = uri?.scheme
        val authority = uri?.authority
        if (uri == null || scheme == null || authority == null) return then(null)
        val origin = "$scheme://$authority"
        // An unspent invite is in the path, and the room cannot say what
        // it is worth unless it is asked about it: with no token and no
        // cookie the answer is `none`, which is also the answer for a
        // page there is no way out of. Sending it is what tells the two
        // apart -- see `door()` in room.svelte.js, which sends the same.
        val token = uri.path?.removePrefix("/")?.let {
            if (it.startsWith("join/")) it.removePrefix("join/").takeIf(String::isNotEmpty) else null
        }
        Thread {
            // A POST, as the page itself asks it -- see `door()` in
            // room.svelte.js.
            val state = runCatching {
                val c = (URL("$origin/api/door").openConnection() as HttpURLConnection).apply {
                    requestMethod = "POST"
                    doOutput = true
                    connectTimeout = 10_000
                    readTimeout = 10_000
                    setRequestProperty("content-type", "application/json")
                    CookieManager.getInstance().getCookie(origin)
                        ?.let { setRequestProperty("Cookie", it) }
                }
                val ask = JSONObject().apply { if (token != null) put("token", token) }
                c.outputStream.use { it.write(ask.toString().toByteArray()) }
                val body = (if (c.responseCode in 200..299) c.inputStream else c.errorStream)
                    ?.bufferedReader()?.use { it.readText() }.orEmpty()
                c.disconnect()
                JSONObject(body).optString("state")
            }.onFailure {
                Log.w(RoomService.TAG, "could not ask the room about the way in: $it")
            }.getOrNull()
            // The state and not the address: an invite link is a
            // credential, and logcat is not where credentials go.
            Log.i(RoomService.TAG, "the way in: ${state ?: "could not ask"}")
            runOnUiThread {
                lastWayIn = state
                then(state)
            }
        }.start()
    }

    /// Write the cookies down whenever this leaves the screen.
    ///
    /// They were only flushed in `onDestroy`, which a force-stop and a
    /// kill for memory both skip -- so a phone that had just joined, and
    /// was then killed rather than closed, came back with no session and
    /// nothing but a spent link to show for it.
    /// Back from the chooser.
    ///
    /// A choice made there that reached the room leaves the page on the
    /// screen still mirroring the old one -- and that mirror is what the
    /// poll reads, so without this the app would overrule its own new
    /// answer with a stale copy until the page next loaded. The flag is
    /// set only when the room actually took it; see [TalkKeyActivity].
    ///
    /// The settings page, if it happens to be open behind this, goes on
    /// showing what it read when it opened. It is right again the next
    /// time it is opened, and it is not where the change was made.
    override fun onResume() {
        super.onResume()
        if (!prefs.getBoolean(TalkKey.PUSH, false)) return
        prefs.edit().remove(TalkKey.PUSH).apply()
        roomVolume = if (TalkKey.volume(prefs)) "on" else "off"
        roomChirp = if (TalkKey.chirp(prefs)) "on" else "off"
        web?.evaluateJavascript(mirrorJs(roomVolume, roomChirp), null)
    }

    /// Going out of the front lets go of the key.
    ///
    /// Not of the call: holding that with the screen off is the whole
    /// point of the service, and nothing here touches it. Only the hold,
    /// which cannot outlive being able to see the key.
    override fun onPause() {
        letGo()
        super.onPause()
    }

    override fun onStop() {
        CookieManager.getInstance().flush()
        super.onStop()
    }

    override fun onDestroy() {
        watching.removeCallbacks(watchTheTalkButton)
        rise?.release(); rise = null
        watchBack(false)
        unregisterReceiver(fromNotification)
        CookieManager.getInstance().flush()
        // Going for good takes the service with it. Not merely going out
        // of sight: being out of sight with the service still up is the
        // entire point of having one, and is what holds the call with the
        // screen off. `isFinishing` is the difference between the two,
        // and without it backing out of the app left a notification
        // saying it was holding a call that nothing was in.
        if (isFinishing) RoomService.stop(this)
        super.onDestroy()
    }
}

/**
 * A WebView that never admits its window went away.
 *
 * Chromium releases a page's audio device about a minute after the
 * window it lives in stops being visible, and the screen turning off is
 * exactly that: the activity is stopped, the view is told its window is
 * gone, and both audio streams are closed underneath a page that is
 * otherwise still running perfectly. The room goes on listing you while
 * hearing silence.
 *
 * The view is what carries that news, so the view is the only place an
 * embedder can decline to carry it. Nothing here touches the page, the
 * audio or the socket. Measured: four minutes of screen-off, no wake
 * lock, not one silent second. See README.md.
 */
/// Shut whatever the room has over itself, one layer, if it has any.
///
/// Back on a phone is the gesture Escape is on a keyboard, and the room
/// already knows what Escape means for each of these -- so this presses
/// Escape rather than reaching in and closing things itself, and
/// whatever the room decides that means is what back means too. A text
/// box with the cursor in it gets the first press, exactly as it would
/// from a keyboard; the next one closes the page it is on.
///
/// A `dialog` is the exception, because a native dialog's Escape is the
/// browser's own and a synthetic key never reaches it. The room answers
/// `cancel` on its dialogs, so that is what is sent.
///
/// Answers `closed` when it took the press, and `nothing` when there
/// was nothing over the room and back means what it meant before.
private const val CLOSE_JS = """
    (() => {
      const dialog = document.querySelector('dialog[open]');
      if (dialog) {
        dialog.dispatchEvent(new Event('cancel', { cancelable: true }));
        return 'closed';
      }
      const OVER = 'main.settings, main.people, section.board, .shade, .menu';
      if (!document.querySelector(OVER)) return 'nothing';
      const at = document.activeElement ?? document.body;
      at.dispatchEvent(new KeyboardEvent('keydown', {
        key: 'Escape', code: 'Escape', bubbles: true, cancelable: true,
      }));
      return 'closed';
    })()
"""

/// Leaving the audio: the room's own way out, so its sockets are let go
/// of the way it lets go of them.
private const val LEAVE_JS = """
    (() => {
      const leave = document.querySelector('.tools button[aria-label="Leave audio"]');
      if (!leave) return 'none';
      leave.click();
      return 'left';
    })()
"""

/// Muting: the one button in the footer that is a mute. Under push to
/// talk the same button is hold-to-talk and pressing it is not a mute,
/// so it is left alone.
private const val MUTE_JS = """
    (() => {
      const b = document.querySelector('footer button.big');
      if (!b || b.classList.contains('ptt') || b.disabled) return 'none';
      b.click();
      return 'pressed';
    })()
"""

/// What the room is doing with your microphone, in one look at the one
/// button it is all written on.
///
/// `says`: what that button says right now, which is what pressing it
/// will do, and empty when pressing it is not a mute. The label is the
/// text beside the key hint rather than the whole of it: the hint is
/// hidden on a touch screen but is still in the text.
///
/// `ptt`: whether that button is the one you hold. It exists only while
/// the audio is joined *and* push to talk is on -- see the footer in
/// `App.svelte` -- so finding it is both halves of the question the
/// phone's talk key has to ask.
///
/// `typing`: whether the focus is in something with a cursor in it. The
/// same selector the room asks it with, from `keys.svelte.js`: the two
/// must not be able to disagree about whose key it is.
///
/// `volume` and `chirp`: what the room has against this person for the
/// talk key, which is a setting on the room's own settings page and is
/// kept on the server, so a second phone arrives already knowing. Read
/// out of storage rather than handed over: the room mirrors it there
/// for exactly this (see `rememberPhone` in `room.svelte.js`), and the
/// app goes on reading the page rather than being called by it. Nothing
/// is injected into the page and there is no way from the page into the
/// app, which matters because this app loads whichever room somebody
/// was invited to and not one it shipped with.
private const val STATE_JS = """
    (() => {
      const TYPING = 'input, textarea, select, [contenteditable=""],' +
        ' [contenteditable="true"], dialog';
      const b = document.querySelector('footer button.big');
      const ptt = !!(b && b.classList.contains('ptt'));
      const typing = !!document.activeElement?.closest?.(TYPING);
      let says = '';
      if (b && !ptt && !b.disabled) {
        const live = b.querySelector('.says > span:not(.ghost)') || b;
        const first = live.childNodes[0];
        says = first ? first.textContent.trim() : '';
      }
      // What the room has against this person for their phone, which
      // beats what this phone was told on its own. Empty for either
      // means nobody has ever said and the app keeps its own answer.
      const volume = localStorage.getItem('vox_room.volume_talk') || '';
      const chirp = localStorage.getItem('vox_room.ptt_chirp') || '';
      return JSON.stringify({ says, ptt, typing, volume, chirp });
    })()
"""

/// Press, or let go of, the key the room's microphone is on.
///
/// A press says first that it has already waited. Push to talk arms for
/// a quarter of a second before the microphone opens -- the chirp fills
/// it; see `lib/chirp.js` -- and this press has spent exactly that long
/// already, deciding whether a volume key was a tap or a hold. Without
/// the mark the room would arm again and a phone would wait twice for
/// one hold.
///
/// Whichever key that is: the room lets you bind another one and keeps
/// the answer in this browser's own storage, so it is read from there
/// rather than assumed to be Space. A binding with modifiers in it is
/// sent with them, because the room's `pressed` wants the key *and* the
/// modifiers and would not fire on the bare key.
///
/// Dispatched at whatever has the focus rather than at the window. The
/// room's handler asks `e.target.closest(...)` whose key this is, and
/// `window` has no `closest` -- an event dispatched there would arrive
/// as a TypeError rather than as a press.
///
/// A press, and only a press, is refused unless the button you hold is
/// actually there. Out here push to talk is known a second late at
/// worst, and the room does something quite different with the talk key
/// when the mode is off: it toggles your mute. A volume key that muted
/// you because the mode had just been switched off is a bad enough
/// trade to be worth one DOM query. The question is asked at the moment
/// of the press rather than off the poll, so there is no window at all.
///
/// Letting go is never refused, for the same reason upside down: the
/// room already drops a hold when the mode goes away under it, and a
/// query that failed for any reason at all would leave a microphone
/// open with nothing on the screen saying so.
private fun talkJs(down: Boolean) = """
    (() => {
      ${if (down) "if (!document.querySelector('footer button.big.ptt')) return;" else ""}
      ${if (down) "try { localStorage.setItem('vox_room.ptt_armed', String(Date.now())); } catch {}" else ""}
      let bind = { code: 'Space', ctrl: false, alt: false, shift: false, meta: false };
      try {
        const raw = JSON.parse(localStorage.getItem('vox_room.talk_key') ?? 'null');
        if (raw && typeof raw.code === 'string' && raw.code) {
          bind = { code: raw.code, ctrl: !!raw.ctrl, alt: !!raw.alt,
                   shift: !!raw.shift, meta: !!raw.meta };
        }
      } catch {}
      const at = document.activeElement ?? document.body;
      at.dispatchEvent(new KeyboardEvent('${if (down) "keydown" else "keyup"}', {
        code: bind.code, ctrlKey: bind.ctrl, altKey: bind.alt,
        shiftKey: bind.shift, metaKey: bind.meta,
        bubbles: true, cancelable: true,
      }));
    })()
"""

/// Put the page's copy of the phone settings right.
///
/// Both values are built from a boolean right here and from nowhere
/// else -- never from the room, never from anything typed -- so neither
/// can be anything but `on` or `off`.
private fun mirrorJs(volume: String, chirp: String) = """
    (() => {
      try {
        localStorage.setItem('vox_room.volume_talk', '$volume');
        localStorage.setItem('vox_room.ptt_chirp', '$chirp');
      } catch {}
    })()
"""

private class KeptVisibleWebView(context: Context) : WebView(context) {
    override fun onWindowVisibilityChanged(visibility: Int) {
        super.onWindowVisibilityChanged(View.VISIBLE)
    }
}

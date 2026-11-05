package com.enigmacurry.vox_room

import android.app.Activity
import android.content.Context
import android.content.SharedPreferences
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.util.TypedValue
import android.view.Gravity
import android.view.KeyEvent
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.LinearLayout
import android.widget.CheckBox
import android.widget.RadioButton
import android.widget.RadioGroup
import android.widget.ScrollView
import android.widget.TextView
import android.webkit.CookieManager
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONObject

/// Where this app's own answers are kept. The room's address lives here
/// too; named once so the two cannot drift apart.
const val PREFS = "room"

/**
 * Which of the phone's physical keys talks.
 *
 * Push to talk is the room's best idea on a keyboard and asks for a key
 * to hold, which is the one thing a phone does not have -- `keys.svelte.js`
 * says so out loud, and the room hides its key hints on a touch screen
 * for the same reason. A phone does have two real buttons, though, and
 * one of them can be lent to the microphone.
 *
 * Only one of them. Taking both would leave no way to turn the room
 * down, and *down* is the one worth keeping: the sound you want gone in
 * a hurry is the room's own, coming out of a speaker, at the moment
 * somebody walks in -- and that is this app in front, so a hijack there
 * really would take the button away when it was wanted. Losing *louder*
 * costs nothing like as much, which is why up is the one offered first.
 *
 * It is this app's answer and not the room's. The room keeps the talk
 * key per browser, deliberately -- it is a fact about a keyboard rather
 * than about a person -- and this is a fact about a phone. Nothing about
 * it goes to the server.
 */
enum class TalkKey(val stored: String, val keyCode: Int, val label: Int) {
    VOLUME_UP("volume_up", KeyEvent.KEYCODE_VOLUME_UP, R.string.talk_key_up),
    VOLUME_DOWN("volume_down", KeyEvent.KEYCODE_VOLUME_DOWN, R.string.talk_key_down),

    /// Neither: both keys stay the phone's, and push to talk on this
    /// phone is the on-screen button alone.
    OFF("off", KeyEvent.KEYCODE_UNKNOWN, R.string.talk_key_off);

    companion object {
        const val PREF = "talk_key"

        /// Whether a hold is announced. On by default: the quarter of a
        /// second a hold waits before it talks is only bearable because
        /// something fills it, and somebody who turns the chirp off
        /// should be doing it having heard what it is for.
        const val CHIRP = "chirp"

        /// Set when a choice made here reached the room, and read by
        /// [MainActivity] on the way back in: the page it is showing
        /// mirrored the *old* answer into its storage, and that mirror
        /// is what the app reads. Without this, a key changed here
        /// would be overruled by a stale copy until the page reloaded.
        const val PUSH = "phone_push"

        /// Volume up, lent to the microphone. On rather than off out of
        /// the box because push to talk with no key to hold is the thing
        /// this setting exists to fix, and because it costs nothing until
        /// push to talk is actually turned on in the room -- see
        /// [MainActivity.dispatchKeyEvent].
        val DEFAULT = VOLUME_UP

        /// By the name it is stored and sent under. Anything else is
        /// [DEFAULT] -- a value from a newer room, or one edited by
        /// hand, must not leave somebody with a talk key that cannot
        /// talk and nothing saying why.
        fun of(stored: String?): TalkKey =
            entries.firstOrNull { it.stored == stored } ?: DEFAULT

        fun read(prefs: SharedPreferences): TalkKey = of(prefs.getString(PREF, null))

        fun write(prefs: SharedPreferences, key: TalkKey) {
            prefs.edit().putString(PREF, key.stored).apply()
        }
    }
}

/**
 * The one thing this app asks that the room cannot be asked.
 *
 * A dialog over whatever is behind it, reached by holding the app's icon
 * -- not a screen inside the app, because the app has no native chrome
 * while a room is up and adding some for a setting nobody looks at twice
 * would be the wrong trade. Opening it does not disturb the call: the
 * WebView declines to notice its window going away (see
 * `KeptVisibleWebView`) and the service goes on holding the room, which
 * is the same arrangement that survives the screen turning off.
 */
class TalkKeyActivity : Activity() {
    override fun onCreate(saved: Bundle?) {
        super.onCreate(saved)
        setTitle(R.string.talk_key_title)
        val prefs = getSharedPreferences(PREFS, MODE_PRIVATE)
        val chosen = TalkKey.read(prefs)
        val dp = { n: Int -> TypedValue.applyDimension(
            TypedValue.COMPLEX_UNIT_DIP, n.toFloat(), resources.displayMetrics).toInt() }

        val blurb = TextView(this).apply {
            text = getString(R.string.talk_key_blurb)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            setPadding(0, 0, 0, dp(16))
        }
        // Ids are the ordinal plus one: a RadioButton with id 0 is a
        // RadioButton with no id, and the group would not keep the
        // check on it.
        val group = RadioGroup(this).apply {
            TalkKey.entries.forEach { key ->
                addView(RadioButton(this@TalkKeyActivity).apply {
                    id = key.ordinal + 1
                    text = getString(key.label)
                    setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
                    setPadding(dp(8), dp(10), 0, dp(10))
                    isChecked = key == chosen
                })
            }
            // Written as it is chosen rather than on the way out, so
            // the answer survives being swiped away instead of pressed
            // through. Done closes; it does not confirm.
            setOnCheckedChangeListener { _, id ->
                TalkKey.entries.getOrNull(id - 1)?.let {
                    TalkKey.write(prefs, it)
                    tellTheRoom()
                }
            }
        }
        val chirp = CheckBox(this).apply {
            text = getString(R.string.talk_key_chirp)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
            setPadding(dp(8), dp(16), 0, 0)
            isChecked = prefs.getBoolean(TalkKey.CHIRP, true)
            setOnCheckedChangeListener { _, on ->
                prefs.edit().putBoolean(TalkKey.CHIRP, on).apply()
                tellTheRoom()
            }
        }
        val done = Button(this).apply {
            text = getString(R.string.talk_key_done)
            val lp = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            lp.topMargin = dp(16)
            layoutParams = lp
            setOnClickListener { finish() }
        }

        said = TextView(this).apply {
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            setPadding(dp(8), dp(12), 0, 0)
            visibility = View.GONE
        }
        val col = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(20), dp(20), dp(20), dp(20))
            addView(blurb); addView(group); addView(chirp); addView(said); addView(done)
        }
        setContentView(ScrollView(this).apply {
            addView(col, ViewGroup.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        })
    }

    private var said: TextView? = null

    /// Write the choice through to the room, where it is kept against
    /// the person rather than against the phone.
    ///
    /// Through to the room and not merely down here, because the room's
    /// answer is the one that wins -- a chooser that only wrote locally
    /// would appear to do nothing to anybody actually in a room, which
    /// is everybody. The same POST the settings page makes, with the
    /// same cookie: this app is already a client of that API, and asking
    /// the server directly is not the page reaching into the app.
    ///
    /// Nothing is undone when it fails. The local answer still stands
    /// for a phone the room has said nothing about, and saying so is
    /// better than a radio button that moves back under the finger.
    private fun tellTheRoom() {
        val prefs = getSharedPreferences(PREFS, MODE_PRIVATE)
        val origin = prefs.getString("url", "").orEmpty()
        val cookie = if (origin.isEmpty()) null
            else CookieManager.getInstance().getCookie(origin)
        if (origin.isEmpty() || cookie.isNullOrEmpty()) return note(R.string.talk_key_phone_only)
        val key = TalkKey.read(prefs).stored
        val chirp = if (prefs.getBoolean(TalkKey.CHIRP, true)) "on" else "off"
        Thread {
            val ok = runCatching {
                val c = (URL("$origin/api/talk").openConnection() as HttpURLConnection).apply {
                    requestMethod = "POST"
                    doOutput = true
                    connectTimeout = 10_000
                    readTimeout = 10_000
                    setRequestProperty("content-type", "application/json")
                    setRequestProperty("Cookie", cookie)
                }
                val ask = JSONObject().put("key", key).put("chirp", chirp)
                c.outputStream.use { it.write(ask.toString().toByteArray()) }
                val good = c.responseCode in 200..299
                c.disconnect()
                good
            }.onFailure {
                Log.w(RoomService.TAG, "could not tell the room about the talk key: $it")
            }.getOrDefault(false)
            Log.i(RoomService.TAG, "the room was told about the talk key: $ok")
            Handler(Looper.getMainLooper()).post {
                if (ok) prefs.edit().putBoolean(TalkKey.PUSH, true).apply()
                note(if (ok) R.string.talk_key_saved else R.string.talk_key_not_saved)
            }
        }.start()
    }

    private fun note(text: Int) {
        said?.apply {
            setText(text)
            visibility = View.VISIBLE
        }
    }
}

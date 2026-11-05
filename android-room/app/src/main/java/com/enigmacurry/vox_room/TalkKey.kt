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
import android.webkit.CookieManager
import android.widget.Button
import android.widget.CheckBox
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONObject

/// Where this app's own answers are kept. The room's address lives here
/// too; named once so the two cannot drift apart.
const val PREFS = "room"

/**
 * Whether this phone's volume keys talk.
 *
 * Push to talk is the room's best idea on a keyboard and asks for a key
 * to hold, which is the one thing a phone does not have -- `keys.svelte.js`
 * says so out loud, and the room hides its key hints on a touch screen
 * for the same reason. A phone does have two real buttons, though, and
 * they can be lent to the microphone.
 *
 * **Both of them, or neither.** It was one of the two at first, on the
 * reasoning that taking a key away costs less on the one that makes the
 * room louder than on the one that makes it quieter in a hurry. Then a
 * tap stopped being taken away at all -- a tap is the volume, a hold is
 * the microphone -- and that reasoning dissolved with it. Lending both
 * costs nothing, so there is nothing to weigh up, and *which one* was a
 * question that only ever made somebody stop and think.
 *
 * The room's answer wins over this one when there is one. See
 * [MainActivity.whichWay].
 */
object TalkKey {
    /// Whether the volume keys talk, and whether holding one chirps.
    /// Both are 'on' or 'off' here -- booleans, stored as the same
    /// words the room stores, so there is one vocabulary and not two.
    const val VOLUME = "volume_talk"
    const val CHIRP = "chirp"

    /// Set when a choice made here reached the room, and read by
    /// [MainActivity] on the way back in: the page it is showing
    /// mirrored the *old* answer into its storage, and that mirror is
    /// what the app reads. Without this, a change made here would be
    /// overruled by a stale copy until the page reloaded.
    const val PUSH = "talk_push"

    /// Both on out of the box. A phone with push to talk on and nothing
    /// to hold is the thing this exists to fix, and neither of them
    /// costs anything until push to talk is actually turned on in the
    /// room -- see [MainActivity.dispatchKeyEvent].
    fun volume(prefs: SharedPreferences) = prefs.getBoolean(VOLUME, true)
    fun chirp(prefs: SharedPreferences) = prefs.getBoolean(CHIRP, true)

    /// What the room says, in the words it says it in.
    fun on(said: String?) = said != "off"
}

/**
 * The two things this app asks that the room cannot be asked.
 *
 * A dialog over whatever is behind it, reached from the notification
 * while a call is up and by holding the app's icon otherwise -- not a
 * screen inside the app, because the app has no native chrome while a
 * room is up. Opening it does not disturb the call: the WebView
 * declines to notice its window going away (see `KeptVisibleWebView`)
 * and the service goes on holding the room, which is the same
 * arrangement that survives the screen turning off.
 *
 * These are the room's settings really. What is here is for a phone
 * that has not joined anything yet, and it writes through to the room
 * the moment it can.
 */
class TalkKeyActivity : Activity() {
    private var said: TextView? = null

    override fun onCreate(saved: Bundle?) {
        super.onCreate(saved)
        setTitle(R.string.talk_key_title)
        val prefs = getSharedPreferences(PREFS, MODE_PRIVATE)
        val dp = { n: Int -> TypedValue.applyDimension(
            TypedValue.COMPLEX_UNIT_DIP, n.toFloat(), resources.displayMetrics).toInt() }

        val blurb = TextView(this).apply {
            text = getString(R.string.talk_key_blurb)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            setPadding(0, 0, 0, dp(16))
        }
        val volume = tick(R.string.talk_key_volume, TalkKey.volume(prefs), dp) { on ->
            prefs.edit().putBoolean(TalkKey.VOLUME, on).apply()
            tellTheRoom()
        }
        val chirp = tick(R.string.talk_key_chirp, TalkKey.chirp(prefs), dp) { on ->
            prefs.edit().putBoolean(TalkKey.CHIRP, on).apply()
            tellTheRoom()
        }
        said = TextView(this).apply {
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            setPadding(dp(8), dp(12), 0, 0)
            visibility = View.GONE
        }
        val done = Button(this).apply {
            text = getString(R.string.talk_key_done)
            val lp = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
            lp.topMargin = dp(16)
            layoutParams = lp
            setOnClickListener { finish() }
        }
        val col = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(20), dp(20), dp(20), dp(20))
            addView(blurb); addView(volume); addView(chirp); addView(said); addView(done)
        }
        setContentView(ScrollView(this).apply {
            addView(col, ViewGroup.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        })
    }

    /// Written as it is ticked rather than on the way out, so the answer
    /// survives being swiped away instead of pressed through. Done
    /// closes; it does not confirm.
    private fun tick(label: Int, on: Boolean, dp: (Int) -> Int, then: (Boolean) -> Unit) =
        CheckBox(this).apply {
            text = getString(label)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
            setPadding(dp(8), dp(10), 0, dp(10))
            isChecked = on
            setOnCheckedChangeListener { _, ticked -> then(ticked) }
        }

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
    /// better than a tick that moves back under the finger.
    private fun tellTheRoom() {
        val prefs = getSharedPreferences(PREFS, MODE_PRIVATE)
        val origin = prefs.getString("url", "").orEmpty()
        val cookie = if (origin.isEmpty()) null
            else CookieManager.getInstance().getCookie(origin)
        if (origin.isEmpty() || cookie.isNullOrEmpty()) return note(R.string.talk_key_phone_only)
        val volume = if (TalkKey.volume(prefs)) "on" else "off"
        val chirp = if (TalkKey.chirp(prefs)) "on" else "off"
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
                val ask = JSONObject().put("volume", volume).put("chirp", chirp)
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

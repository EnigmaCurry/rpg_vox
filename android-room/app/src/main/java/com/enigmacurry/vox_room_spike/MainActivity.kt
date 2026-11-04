package com.enigmacurry.vox_room_spike

import android.Manifest
import android.annotation.SuppressLint
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import android.view.View
import android.view.ViewGroup
import android.webkit.CookieManager
import android.webkit.PermissionRequest
import android.webkit.WebChromeClient
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import android.app.Activity

/**
 * A WebView pointed at a voice room, held up by a foreground service.
 *
 * The question: does the room's audio keep going with the screen off?
 * Everything the room needs is already in the web app -- this adds no
 * audio code at all. What it adds is the one thing a browser tab cannot
 * have, which is a foreground service of its own.
 *
 * It also measures the thing the audio was moved off. Every second it
 * asks the page what time it thinks it is; the answer says whether the
 * page's main thread is being scheduled, and how late. A main thread
 * that stalls while the room still sounds is the worker earning its
 * keep (see web/src/lib/audio.worker.js) -- so the two halves are
 * reported separately on purpose.
 *
 * Whether the audio itself kept flowing is not measured here. The
 * server already knows: it writes a level for every person in the room
 * ten times a second, so somebody watching from a laptop sees this
 * phone's level go flat the moment its frames stop arriving. Keeping
 * the measurement outside the thing being measured is the point.
 */
class MainActivity : Activity() {
    private var web: WebView? = null
    private val beat = Handler(Looper.getMainLooper())
    /// What the page last said the time was.
    private var pageSaid = 0L

    private val prefs by lazy { getSharedPreferences("spike", MODE_PRIVATE) }

    override fun onCreate(saved: Bundle?) {
        super.onCreate(saved)
        val want = mutableListOf(Manifest.permission.RECORD_AUDIO)
        if (Build.VERSION.SDK_INT >= 33) want += Manifest.permission.POST_NOTIFICATIONS
        val missing = want.filter {
            checkSelfPermission(it) != PackageManager.PERMISSION_GRANTED
        }
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
            Log.w(RoomService.TAG, "no microphone: nothing to hold open")
            finish()
            return
        }
        begin()
    }

    /// `singleTask`, so a second launch is an intent into the instance
    /// that is already here rather than a new one. A run pointed at a
    /// different link should follow the link.
    override fun onNewIntent(next: Intent) {
        super.onNewIntent(next)
        intent = next
        val given = next.getStringExtra("url")?.trim()
        if (!given.isNullOrEmpty()) {
            Log.i(RoomService.TAG, "new link, starting over")
            web?.destroy()
            web = null
            begin()
        }
    }

    /// Launchable from adb, so a run of this can be scripted:
    ///   am start -n <pkg>/.MainActivity -e url <link> --ez wakelock false
    private fun begin() {
        val given = intent?.getStringExtra("url")?.trim()
        if (!given.isNullOrEmpty()) {
            val lock = intent?.getBooleanExtra("wakelock", false) ?: false
            prefs.edit().putString("url", given).putBoolean("wakelock", lock).apply()
            open(given, lock)
        } else {
            door()
        }
    }

    /// The URL to open and whether to hold a wake lock. Both are
    /// remembered, so a second run is one button.
    private fun door() {
        val pad = (16 * resources.displayMetrics.density).toInt()
        val url = EditText(this).apply {
            hint = "https://room.example.com"
            setText(prefs.getString("url", ""))
        }
        val lock = CheckBox(this).apply {
            text = "Hold a partial wake lock"
            isChecked = prefs.getBoolean("wakelock", false)
        }
        val go = Button(this).apply { text = "Open the room" }
        val why = TextView(this).apply {
            text = "The room must be https: a browser only gives the " +
                "microphone to a secure page, and a LAN http:// address is not one."
        }
        val col = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, pad, pad, pad)
            addView(url)
            addView(lock)
            addView(go)
            addView(why)
        }
        setContentView(col)
        go.setOnClickListener {
            val where = url.text.toString().trim()
            if (where.isEmpty()) return@setOnClickListener
            prefs.edit()
                .putString("url", where)
                .putBoolean("wakelock", lock.isChecked)
                .apply()
            open(where, lock.isChecked)
        }
    }

    @SuppressLint("SetJavaScriptEnabled")
    private fun open(url: String, wakeLock: Boolean) {
        RoomService.start(this, wakeLock)
        CookieManager.getInstance().setAcceptCookie(true)
        val w = KeptVisibleWebView(this)
        web = w
        WebView.setWebContentsDebuggingEnabled(true)
        w.settings.apply {
            javaScriptEnabled = true
            // The theme is read out of localStorage before anything is
            // drawn, and the invite is spent for a cookie.
            domStorageEnabled = true
            // The room starts its AudioContext from the Join Audio click,
            // but the click is the page's, not a gesture this WebView
            // will vouch for. Without this the context stays suspended.
            mediaPlaybackRequiresUserGesture = false
        }
        w.webChromeClient = object : WebChromeClient() {
            override fun onPermissionRequest(request: PermissionRequest) {
                // The page asking for the microphone is the whole reason
                // the service above is microphone-typed.
                val mic = request.resources.filter {
                    it == PermissionRequest.RESOURCE_AUDIO_CAPTURE
                }
                if (mic.isEmpty()) return request.deny()
                runOnUiThread { request.grant(mic.toTypedArray()) }
            }

            override fun onConsoleMessage(m: android.webkit.ConsoleMessage): Boolean {
                // The worker's own warnings come up here too, which is
                // where we would see WebCodecs failing in a WebView.
                Log.i(RoomService.TAG, "page: ${m.message()}")
                return true
            }
        }
        w.webViewClient = object : WebViewClient() {
            override fun onPageFinished(view: WebView, url: String) {
                // Press Join Audio for us, so a run of this needs no hands.
                // The room only starts its audio from that click, and
                // `mediaPlaybackRequiresUserGesture = false` above is what
                // lets a click we made ourselves count as one.
                if (!intent.getBooleanExtra("autojoin", false)) return
                view.postDelayed({
                    view.evaluateJavascript(
                        """(() => {
                          const tick = document.querySelector('.consent input');
                          if (tick && !tick.checked) tick.click();
                          const join = [...document.querySelectorAll('button')]
                            .find(b => /join audio/i.test(b.textContent));
                          if (join) { join.click(); return 'joined'; }
                          return 'no join button';
                        })()"""
                    ) { r -> Log.i(RoomService.TAG, "autojoin: " + r) }
                }, 1500)
            }
        }
        setContentView(w, ViewGroup.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        w.loadUrl(url)
        heartbeat()
    }

    /// Ask the page the time, once a second, and say how late the answer
    /// was. `lag` is the page's own clock against ours: a main thread
    /// being throttled falls behind, and by how much.
    private fun heartbeat() {
        beat.removeCallbacksAndMessages(null)
        val tick = object : Runnable {
            override fun run() {
                val w = web
                if (w != null) {
                    val sent = SystemClock.elapsedRealtime()
                    w.evaluateJavascript("Date.now()") { value ->
                        val round = SystemClock.elapsedRealtime() - sent
                        // evaluateJavascript hands numbers back as JSON, and
                        // a millisecond clock comes out in exponent form
                        // (1.7938e+12), which no integer parse will take.
                        val said = value?.trim('"', ' ')?.toDoubleOrNull()?.toLong()
                        if (said == null) {
                            Log.i(RoomService.TAG, "beat round=${round}ms page said $value")
                        } else {
                            val gap = if (pageSaid == 0L) -1L else said - pageSaid
                            pageSaid = said
                            Log.i(RoomService.TAG,
                                "beat round=${round}ms page-advanced=${gap}ms")
                        }
                    }
                }
                beat.postDelayed(this, 1000)
            }
        }
        beat.post(tick)
    }

    override fun onDestroy() {
        beat.removeCallbacksAndMessages(null)
        // Deliberately NOT w.onPause()/pauseTimers(): that throttling is
        // opt-in, and opting in is the one thing this app must not do.
        CookieManager.getInstance().flush()
        super.onDestroy()
    }
}

/**
 * A WebView that never admits its window went away.
 *
 * Chromium suspends a page's media when the window it lives in stops
 * being visible, and the screen turning off is exactly that: the
 * activity is stopped, the view is told its window is gone, and about a
 * minute later the audio streams are closed underneath a page that is
 * otherwise still running perfectly.
 *
 * The view is what carries that news, so the view is the only place an
 * embedder can decline to carry it. Nothing here touches the page, the
 * audio or the socket -- it only declines to say the window is hidden.
 */
private class KeptVisibleWebView(context: android.content.Context) : WebView(context) {
    override fun onWindowVisibilityChanged(visibility: Int) {
        super.onWindowVisibilityChanged(View.VISIBLE)
    }
}

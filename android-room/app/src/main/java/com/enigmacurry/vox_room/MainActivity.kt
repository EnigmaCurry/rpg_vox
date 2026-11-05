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
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.util.TypedValue
import android.view.Gravity
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
import org.json.JSONObject

/**
 * A voice room in a WebView, held up by a foreground service.
 *
 * The app ships no address. Whose room this is belongs to whoever
 * installed it, so the first screen asks, and the answer is kept. That
 * is the whole of the native UI; everything after it is the room's own.
 */
class MainActivity : Activity() {
    private var web: WebView? = null
    private val prefs by lazy { getSharedPreferences("room", MODE_PRIVATE) }

    /// Leaving from the notification has to reach the WebView, which the
    /// service cannot touch.
    private val leaving = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) = setup()
    }

    override fun onCreate(saved: Bundle?) {
        super.onCreate(saved)
        registerReceiver(leaving, IntentFilter("leave"), RECEIVER_NOT_EXPORTED)
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

    private fun begin() {
        // `-e url` lets a run of this be scripted; see README.md.
        val given = intent?.getStringExtra("url")?.trim()
        val where = given?.takeIf { it.isNotEmpty() } ?: prefs.getString("url", "").orEmpty()
        if (where.isEmpty()) setup() else enter(where)
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
        web?.destroy()
        web = null
        enter(given?.takeIf { it.isNotEmpty() } ?: prefs.getString("url", "").orEmpty())
    }

    // -- the one screen this app has of its own ------------------------

    /// Ask where the room is. Shown on the first run, when the room is
    /// left from the notification, and whenever Back reaches the door.
    private fun setup(problem: String? = null) {
        RoomService.stop(this)
        watchBack(false)
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
            setText(prefs.getString("url", ""))
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
                    prefs.edit().putString("url", said.url).apply()
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

    private fun back() {
        val w = web ?: return
        if (w.canGoBack()) w.goBack() else setup()
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
        val uri = runCatching { Uri.parse(pageUrl) }.getOrNull() ?: return
        val scheme = uri.scheme ?: return
        val authority = uri.authority ?: return
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
            }.getOrNull() ?: return@Thread
            // The state and not the address: an invite link is a
            // credential, and logcat is not where credentials go.
            Log.i(RoomService.TAG, "the way in: $state")
            val why = when (state) {
                "none" -> getString(R.string.need_link)
                "expired" -> getString(R.string.link_spent)
                else -> return@Thread
            }
            runOnUiThread { if (web != null) setup(why) }
        }.start()
    }

    override fun onDestroy() {
        watchBack(false)
        unregisterReceiver(leaving)
        CookieManager.getInstance().flush()
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
private class KeptVisibleWebView(context: Context) : WebView(context) {
    override fun onWindowVisibilityChanged(visibility: Int) {
        super.onWindowVisibilityChanged(View.VISIBLE)
    }
}

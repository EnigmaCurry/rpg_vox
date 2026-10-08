package com.enigmacurry.rpg_vox_scribe

import android.app.ForegroundServiceStartNotAllowedException
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.SystemClock
import android.util.Log

/**
 * Foreground service that keeps the process alive: with the microphone
 * and/or a media projection (phone audio) while a recording is open, so
 * it continues in the background and with the screen off; while a file
 * is transcribed; then idle with the models loaded, until the app has
 * been out of sight for [IDLE_MS]. The notification shows the time and
 * the latest finished line, with Pause/Resume and Stop.
 */
class ScribeService : Service() {
    companion object {
        private const val TAG = "Scribe"
        private const val CHANNEL = "dictation"
        private const val ID = 2
        private const val IDLE_MS = 10 * 60_000L
        private const val ACTION_PAUSE = "pause"
        private const val ACTION_RESUME = "resume"
        private const val ACTION_STOP = "stop"
        private const val ACTION_UNLOAD = "unload"

        /** Start or refresh the service for [Scribe]'s current state. Must be called from the app. */
        fun update(context: Context) {
            val intent = Intent(context, ScribeService::class.java)
            try {
                context.startForegroundService(intent)
            } catch (e: Exception) {
                // Not allowed from the background (Android 12+): the app was left
                // while the models loaded. They stay loaded until the process goes.
                if (Build.VERSION.SDK_INT >= 31 && e is ForegroundServiceStartNotAllowedException) {
                    Log.w(TAG, "service not started from background")
                } else {
                    throw e
                }
            }
        }
    }

    private val handler = Handler(Looper.getMainLooper())
    private var shown = ""
    private var types = -1

    private val tick = object : Runnable {
        override fun run() {
            refresh()
            handler.postDelayed(this, 1_000)
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        Scribe.init(this)
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, "Dictation", NotificationManager.IMPORTANCE_LOW).apply {
                setShowBadge(false)
            },
        )
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_PAUSE -> Scribe.pause()
            ACTION_RESUME -> Scribe.resume()
            ACTION_STOP -> Scribe.stop()
            ACTION_UNLOAD -> Scribe.release()
        }
        // Every start must reach startForeground, even one that is about to stop.
        foreground(force = true)
        handler.removeCallbacks(tick)
        tick.run()
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        handler.removeCallbacks(tick)
        super.onDestroy()
    }

    private fun refresh() {
        val busy = Scribe.busy()
        val idleFor = SystemClock.elapsedRealtime() - Scribe.lastActive
        if (!busy && !Scribe.visible && idleFor > IDLE_MS) Scribe.release()
        if (!busy && !Scribe.loaded()) {
            handler.removeCallbacks(tick)
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return
        }
        foreground(force = false)
    }

    /** The foreground service types the current state needs. */
    private fun wantedTypes(): Int {
        var t = 0
        if (Scribe.recording && !Scribe.finishing) {
            if (Scribe.recordingMode.mic) t = t or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
            if (Scribe.recordingMode.phone) t = t or ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION
        }
        return if (t == 0) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else t
    }

    /** Post the notification; switch the service types when recording starts or ends. */
    private fun foreground(force: Boolean) {
        val n = build()
        val want = wantedTypes()
        if (force || want != types) {
            types = want
            when {
                Build.VERSION.SDK_INT >= 34 -> startForeground(ID, n.first, want)
                Build.VERSION.SDK_INT >= 29 && want != ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE ->
                    startForeground(ID, n.first, want)
                else -> startForeground(ID, n.first)
            }
            shown = n.second
            // Phone audio: the projection may only be created once this is a
            // mediaProjection foreground service (Android 14+).
            Scribe.pendingProjection?.let { (code, data) ->
                if (want and ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION != 0) {
                    val mpm = getSystemService(MediaProjectionManager::class.java)
                    val mp = runCatching { mpm.getMediaProjection(code, data) }
                        .onFailure { Log.e(TAG, "media projection failed", it) }
                        .getOrNull()
                    if (mp != null) Scribe.onProjection(mp) else Scribe.stop()
                }
            }
        } else if (n.second != shown) {
            shown = n.second
            getSystemService(NotificationManager::class.java).notify(ID, n.first)
        }
    }

    /** The notification and a key of what it shows, to skip identical updates. */
    private fun build(): Pair<Notification, String> {
        val open = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val time = clock(Scribe.elapsedMs())
        val line = Scribe.lastLine().let { if (it.length > 120) "…" + it.takeLast(119) else it }
        val what = when (Scribe.recordingMode) {
            Mode.MIC -> "Dictating"
            Mode.PHONE -> "Transcribing phone audio"
            Mode.BOTH -> "Dictating + phone audio"
        }
        val job = Scribe.file
        val (title, text) = when {
            job != null -> "Transcribing ${job.name}" to
                (job.detail()?.let { "$it · " } ?: if (job.progress >= 0) "${(job.progress * 100).toInt()}% · " else "") + line
            Scribe.finishing -> "Finishing transcription…" to line
            Scribe.recording && Scribe.paused -> "Paused · $time" to line
            Scribe.recording -> "$what · $time" to line.ifEmpty { "Listening…" }
            else -> "Ready to dictate" to "Speech models loaded · tap to open"
        }
        val b = Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_btn_speak_now)
            .setContentTitle(title)
            .setContentText(text)
            .setStyle(Notification.BigTextStyle().bigText(text))
            .setContentIntent(open)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setCategory(Notification.CATEGORY_SERVICE)
        if (Build.VERSION.SDK_INT >= 31) {
            b.setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE)
        }
        if (job != null && job.progress >= 0) b.setProgress(100, (job.progress * 100).toInt(), false)
        when {
            job != null -> b.addAction(action(ACTION_STOP, "Cancel"))
            Scribe.finishing -> {}
            Scribe.recording -> {
                if (Scribe.paused) b.addAction(action(ACTION_RESUME, "Resume"))
                else b.addAction(action(ACTION_PAUSE, "Pause"))
                b.addAction(action(ACTION_STOP, "Stop"))
            }
            else -> b.addAction(action(ACTION_UNLOAD, "Unload models"))
        }
        return b.build() to "$title|$text|${Scribe.paused}|${Scribe.recording}|${job != null}"
    }

    private fun action(name: String, label: String): Notification.Action {
        val pi = PendingIntent.getService(
            this, name.hashCode(),
            Intent(this, ScribeService::class.java).setAction(name),
            PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Action.Builder(null, label, pi).build()
    }

    private fun clock(ms: Long): String {
        val s = ms / 1000
        return if (s >= 3600) "%d:%02d:%02d".format(s / 3600, s / 60 % 60, s % 60)
        else "%d:%02d".format(s / 60, s % 60)
    }
}

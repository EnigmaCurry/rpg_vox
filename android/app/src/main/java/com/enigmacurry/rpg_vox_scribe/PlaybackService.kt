package com.enigmacurry.rpg_vox_scribe

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.media.MediaMetadata
import android.media.session.MediaSession
import android.media.session.PlaybackState
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.compose.runtime.snapshotFlow
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch

/**
 * Foreground service (media playback) while a thread plays: keeps the
 * process alive with the app left or the screen off, and puts a media
 * session with back 10 s / pause / ahead 10 s / stop in the notification
 * and on the lock screen. It also holds audio focus, pausing when
 * another app takes it or headphones are unplugged.
 */
class PlaybackService : Service() {
    companion object {
        private const val TAG = "Playback"
        private const val CHANNEL = "playback"
        private const val ID = 3
        private const val ACTION_BACK = "back"
        private const val ACTION_TOGGLE = "toggle"
        private const val ACTION_AHEAD = "ahead"
        private const val ACTION_STOP = "stop"
        private const val SKIP_MS = 10_000L

        /** Start (or refresh) the service; from the app, once playback has started. */
        fun start(context: Context) {
            runCatching { context.startForegroundService(Intent(context, PlaybackService::class.java)) }
                .onFailure { Log.w(TAG, "playback service not started", it) }
        }
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private lateinit var session: MediaSession
    private lateinit var audio: AudioManager
    private var focus: AudioFocusRequest? = null

    /** Headphones unplugged (or Bluetooth gone): pause rather than play out loud. */
    private val noisy = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action == AudioManager.ACTION_AUDIO_BECOMING_NOISY) Playback.pause()
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, "Playback", NotificationManager.IMPORTANCE_LOW).apply {
                setShowBadge(false)
            },
        )
        audio = getSystemService(AudioManager::class.java)
        session = MediaSession(this, "Scribe").apply {
            setCallback(object : MediaSession.Callback() {
                override fun onPlay() = Playback.resume()
                override fun onPause() = Playback.pause()
                override fun onStop() = Playback.stop()
                override fun onRewind() = Playback.skip(-SKIP_MS)
                override fun onFastForward() = Playback.skip(SKIP_MS)
                override fun onSkipToPrevious() = Playback.skip(-SKIP_MS)
                override fun onSkipToNext() = Playback.skip(SKIP_MS)
                override fun onSeekTo(pos: Long) = Playback.seekTo(pos)
            })
            setSessionActivity(openApp())
            isActive = true
        }
        registerReceiver(noisy, IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY))
        requestFocus()
        // Follow the player: refresh the session and notification on any
        // change, and leave once it stops.
        scope.launch {
            snapshotFlow { Triple(Playback.playing, Playback.paused, Playback.lengthMs to Playback.seeks) }
                .collect { (playing, _, _) ->
                    if (playing == null) {
                        stopForeground(STOP_FOREGROUND_REMOVE)
                        stopSelf()
                    } else {
                        publish()
                    }
                }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_BACK -> Playback.skip(-SKIP_MS)
            ACTION_TOGGLE -> if (Playback.paused) Playback.resume() else Playback.pause()
            ACTION_AHEAD -> Playback.skip(SKIP_MS)
            ACTION_STOP -> Playback.stop()
        }
        // Every start must reach startForeground, even one about to stop.
        val n = notification()
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
        } else {
            startForeground(ID, n)
        }
        if (Playback.playing == null) {
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        scope.cancel()
        runCatching { unregisterReceiver(noisy) }
        focus?.let { audio.abandonAudioFocusRequest(it) }
        session.isActive = false
        session.release()
        super.onDestroy()
    }

    private fun requestFocus() {
        val req = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                    .build(),
            )
            .setOnAudioFocusChangeListener { change ->
                // A call, another player, a navigation prompt: hold here.
                if (change == AudioManager.AUDIOFOCUS_LOSS || change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT) {
                    Playback.pause()
                }
            }
            .build()
        focus = req
        audio.requestAudioFocus(req)
    }

    /** The session's state and metadata, and the notification, as the player is now. */
    private fun publish() {
        val playing = !Playback.paused
        session.setPlaybackState(
            PlaybackState.Builder()
                .setActions(
                    PlaybackState.ACTION_PLAY or PlaybackState.ACTION_PAUSE or PlaybackState.ACTION_PLAY_PAUSE or
                        PlaybackState.ACTION_STOP or PlaybackState.ACTION_REWIND or PlaybackState.ACTION_FAST_FORWARD or
                        PlaybackState.ACTION_SKIP_TO_PREVIOUS or PlaybackState.ACTION_SKIP_TO_NEXT or
                        PlaybackState.ACTION_SEEK_TO,
                )
                .setState(
                    if (playing) PlaybackState.STATE_PLAYING else PlaybackState.STATE_PAUSED,
                    Playback.positionMs(),
                    if (playing) 1f else 0f,
                )
                .build(),
        )
        session.setMetadata(
            MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, Scribe.thread.title)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, "Scribe")
                .putLong(MediaMetadata.METADATA_KEY_DURATION, Playback.lengthMs)
                .build(),
        )
        getSystemService(NotificationManager::class.java).notify(ID, notification())
    }

    private fun notification(): Notification {
        val playing = !Playback.paused
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_media_play)
            .setContentTitle(Scribe.thread.title)
            .setContentText(if (playing) "Playing" else "Paused")
            .setContentIntent(openApp())
            .setDeleteIntent(action(ACTION_STOP))
            .setOngoing(playing)
            .setVisibility(Notification.VISIBILITY_PUBLIC)
            .addAction(button(android.R.drawable.ic_media_rew, "Back 10 seconds", ACTION_BACK))
            .addAction(
                if (playing) {
                    button(android.R.drawable.ic_media_pause, "Pause", ACTION_TOGGLE)
                } else {
                    button(android.R.drawable.ic_media_play, "Resume", ACTION_TOGGLE)
                },
            )
            .addAction(button(android.R.drawable.ic_media_ff, "Ahead 10 seconds", ACTION_AHEAD))
            .addAction(button(android.R.drawable.ic_menu_close_clear_cancel, "Stop", ACTION_STOP))
            .setStyle(
                Notification.MediaStyle()
                    .setMediaSession(session.sessionToken)
                    .setShowActionsInCompactView(0, 1, 2),
            )
            .build()
    }

    private fun button(icon: Int, label: String, act: String) =
        Notification.Action.Builder(Icon.createWithResource(this, icon), label, action(act)).build()

    private fun action(act: String): PendingIntent =
        PendingIntent.getService(
            this, act.hashCode(),
            Intent(this, PlaybackService::class.java).setAction(act),
            PendingIntent.FLAG_IMMUTABLE,
        )

    private fun openApp(): PendingIntent =
        PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
}

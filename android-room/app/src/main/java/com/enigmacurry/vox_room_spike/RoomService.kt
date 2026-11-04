package com.enigmacurry.vox_room_spike

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log

/**
 * The reason this spike exists: a microphone-typed foreground service,
 * so Android keeps the process scheduled while the screen is off and
 * the WebView inside it goes on holding a call.
 *
 * Scribe's recording survives on exactly this (see ScribeService in
 * ../../android), so the machinery is known to work for a microphone
 * the app itself opens. What is not known is whether it is enough for a
 * microphone a *WebView* opened, with the audio loop in a worker.
 *
 * The wake lock is deliberately optional and off by default. A
 * foreground service is not a promise of CPU -- the device can still
 * doze -- and active audio capture usually keeps it awake on its own.
 * Which of those is true here is half of what we are measuring, so the
 * naive version is the one that runs first.
 */
class RoomService : Service() {
    companion object {
        const val TAG = "VoxSpike"
        private const val CHANNEL = "room"
        private const val ID = 1
        const val EXTRA_WAKELOCK = "wakelock"
        private const val ACTION_STOP = "stop"

        fun start(context: Context, wakeLock: Boolean) {
            val intent = Intent(context, RoomService::class.java)
                .putExtra(EXTRA_WAKELOCK, wakeLock)
            context.startForegroundService(intent)
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, RoomService::class.java))
        }
    }

    private var lock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        val nm = getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= 26 && nm.getNotificationChannel(CHANNEL) == null) {
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL, "In the room", NotificationManager.IMPORTANCE_LOW)
            )
        }
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val stop = PendingIntent.getService(
            this, 1, Intent(this, RoomService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val note: Notification = Notification.Builder(this, CHANNEL)
            .setContentTitle("In the voice room")
            .setContentText("Holding the call with the screen off")
            .setSmallIcon(android.R.drawable.ic_btn_speak_now)
            .setContentIntent(open)
            .setOngoing(true)
            .addAction(Notification.Action.Builder(null, "Leave", stop).build())
            .build()
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(ID, note, ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE)
        } else {
            startForeground(ID, note)
        }

        if (intent?.getBooleanExtra(EXTRA_WAKELOCK, false) == true && lock == null) {
            lock = getSystemService(PowerManager::class.java)
                .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "VoxSpike:room")
                .also { it.acquire() }
            Log.i(TAG, "service up, partial wake lock held")
        } else {
            Log.i(TAG, "service up, no wake lock")
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        lock?.let { if (it.isHeld) it.release() }
        lock = null
        Log.i(TAG, "service down")
        super.onDestroy()
    }
}

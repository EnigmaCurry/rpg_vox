package com.enigmacurry.vox_room

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import android.util.Log

/**
 * What a browser tab cannot have: a foreground service of this app's
 * own, typed for the microphone, so Android keeps the process scheduled
 * and the WebView inside it goes on holding the call with the screen
 * off.
 *
 * There is deliberately no wake lock. One was tried and measured and it
 * changes nothing -- a wake lock guarantees the CPU, and the CPU was
 * never what went missing. What goes missing is the audio device, and
 * the answer to that is in [MainActivity]'s WebView, not here. See
 * README.md.
 */
class RoomService : Service() {
    companion object {
        const val TAG = "VoxRoom"
        private const val CHANNEL = "room"
        private const val ID = 1
        private const val ACTION_LEAVE = "leave"

        fun start(context: Context) {
            context.startForegroundService(Intent(context, RoomService::class.java))
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, RoomService::class.java))
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_LEAVE) {
            // Leaving from the notification closes the room as well as the
            // service: a call you dismissed should not go on listening.
            sendBroadcast(Intent(ACTION_LEAVE).setPackage(packageName))
            stopSelf()
            return START_NOT_STICKY
        }
        val nm = getSystemService(NotificationManager::class.java)
        if (nm.getNotificationChannel(CHANNEL) == null) {
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL, getString(R.string.note_title),
                    NotificationManager.IMPORTANCE_LOW)
            )
        }
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val leave = PendingIntent.getService(
            this, 1, Intent(this, RoomService::class.java).setAction(ACTION_LEAVE),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val note: Notification = Notification.Builder(this, CHANNEL)
            .setContentTitle(getString(R.string.note_title))
            .setContentText(getString(R.string.note_text))
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentIntent(open)
            .setOngoing(true)
            .addAction(Notification.Action.Builder(
                null, getString(R.string.note_leave), leave).build())
            .build()
        startForeground(ID, note, ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE)
        Log.i(TAG, "holding the room")
        // Restarting this without the WebView it exists for would leave a
        // notification over nothing.
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        Log.i(TAG, "let the room go")
        super.onDestroy()
    }
}

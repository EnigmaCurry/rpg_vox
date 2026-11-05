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
        const val ACTION_LEAVE = "leave"
        const val ACTION_MUTE = "mute"
        private const val EXTRA_SAYS = "says"

        fun start(context: Context) {
            context.startForegroundService(Intent(context, RoomService::class.java))
        }

        /// What the room's own talk button says, so the notification can
        /// offer the same press. Empty while there is nothing to mute --
        /// before the audio is joined, or under push to talk, where the
        /// microphone is already shut unless something is being held.
        fun says(context: Context, label: String) {
            context.startForegroundService(
                Intent(context, RoomService::class.java).putExtra(EXTRA_SAYS, label))
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, RoomService::class.java))
        }
    }

    /// What the room's talk button says, or empty for no mute action.
    private var says = ""

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // Both of these are the activity's to carry out: the room is a
        // page, and pressing its buttons is the only way to work it.
        // Leaving also ends this service -- but the activity says when,
        // once the room has actually been left.
        if (intent?.action == ACTION_LEAVE || intent?.action == ACTION_MUTE) {
            sendBroadcast(Intent(intent.action).setPackage(packageName))
            return START_NOT_STICKY
        }
        intent?.getStringExtra(EXTRA_SAYS)?.let { says = it }
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
        val mute = PendingIntent.getService(
            this, 2, Intent(this, RoomService::class.java).setAction(ACTION_MUTE),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        // Which volume key talks, reachable while a call is up.
        //
        // It is on the launcher's long-press menu as well, which is
        // where Android puts a setting an app has no room for -- but
        // nobody looks there, and the setting is about a key you only
        // ever reach for mid-conversation. This is the app's native
        // controls, so this is where it goes. Straight to the activity
        // rather than through here: it opens over the room and has
        // nothing to ask the service.
        val talkKey = PendingIntent.getActivity(
            this, 3, Intent(this, TalkKeyActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val note: Notification = Notification.Builder(this, CHANNEL)
            .setContentTitle(getString(R.string.note_title))
            .setContentText(getString(R.string.note_text))
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentIntent(open)
            .setOngoing(true)
            // The room's own word for it, so the notification offers
            // the same press as the button behind it rather than a
            // second vocabulary for the same microphone.
            .also { b ->
                if (says.isNotEmpty()) {
                    b.addAction(Notification.Action.Builder(null, says, mute).build())
                }
            }
            .addAction(Notification.Action.Builder(
                null, getString(R.string.note_leave), leave).build())
            // Last of the three Android will show. Under push to talk
            // there is no mute to offer and this is the second of two,
            // which is the moment it is most worth having.
            .addAction(Notification.Action.Builder(
                null, getString(R.string.talk_key_title), talkKey).build())
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

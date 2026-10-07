package com.enigmacurry.voxscribe

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
import android.util.Log
import kotlin.concurrent.thread
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

sealed interface Install {
    data object Idle : Install
    data class Running(val step: Step, val overall: Float) : Install
    data class Failed(val error: String) : Install
    data object Done : Install
}

/**
 * Downloads and unpacks the models in a foreground service, so the
 * install keeps going while the app is in the background. Progress is
 * published on [state] for the UI.
 */
class InstallService : Service() {
    companion object {
        private const val TAG = "VoxScribe"
        private const val CHANNEL = "install"
        private const val ID = 1

        private val _state = MutableStateFlow<Install>(Install.Idle)
        val state: StateFlow<Install> = _state.asStateFlow()

        fun start(context: Context) {
            if (_state.value is Install.Running) return
            _state.value = Install.Running(Step.Downloading(BUNDLES.first().dir, 0, 1), 0f)
            context.startForegroundService(Intent(context, InstallService::class.java))
        }
    }

    private var worker: Thread? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL, "Model download", NotificationManager.IMPORTANCE_LOW),
        )
        val notification = notify(_state.value)
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        } else {
            startForeground(ID, notification)
        }
        if (worker?.isAlive == true) return START_NOT_STICKY
        worker = thread(name = "vox-install") { run(nm) }
        return START_NOT_STICKY
    }

    private fun run(nm: NotificationManager) {
        val store = ModelStore(noBackupFilesDir)
        val all = BUNDLES.sumOf { it.approxBytes }.toFloat()
        var shown = -1
        val result = runCatching {
            store.install { step ->
                // Bundles install in order: everything before this one is done.
                val dir = when (step) {
                    is Step.Downloading -> step.bundle
                    is Step.Unpacking -> step.bundle
                }
                val before = BUNDLES.takeWhile { it.dir != dir }.sumOf { it.approxBytes }
                // Downloading is most of the wait; unpacking is the rest of the bar's last stretch.
                val here = when (step) {
                    is Step.Downloading -> step.done.toFloat()
                    is Step.Unpacking -> BUNDLES.first { it.dir == dir }.approxBytes.toFloat()
                }
                val s = Install.Running(step, ((before + here) / all).coerceIn(0f, 1f))
                _state.value = s
                val pct = (s.overall * 100).toInt() * 1000 + when (step) {
                    is Step.Downloading -> 0
                    is Step.Unpacking -> 1 + (step.fraction * 100).toInt()
                }
                if (pct != shown) {
                    shown = pct
                    nm.notify(ID, notify(s))
                }
            }
        }
        _state.value = result.fold(
            onSuccess = { Install.Done },
            onFailure = {
                Log.e(TAG, "model install failed", it)
                Install.Failed(it.message ?: it.toString())
            },
        )
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun notify(s: Install): Notification {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        val b = Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle("Downloading speech models")
            .setContentIntent(open)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
        if (s is Install.Running) {
            b.setContentText(describe(s.step)).setProgress(1000, (s.overall * 1000).toInt(), false)
        }
        return b.build()
    }
}

fun describe(step: Step): String = when (step) {
    is Step.Downloading -> "${step.bundle}: ${step.done / 1_000_000} of ${step.total / 1_000_000} MB"
    is Step.Unpacking -> "Unpacking ${step.bundle}… ${(step.fraction * 100).toInt()}%"
}

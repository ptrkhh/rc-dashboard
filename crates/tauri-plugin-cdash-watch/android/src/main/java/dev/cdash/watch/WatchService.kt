package dev.cdash.watch

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL

/** The service's own notification uses id 1 with no tag; each note uses id 1 tagged with its session. */
const val NOTE_ID = 1
private const val CHANNEL_BACKGROUND = "background"

/**
 * Holds `<base>/api/notify` open and posts each note. Started by the switch,
 * never at boot; Android restarts it (START_STICKY) with a null intent, so the
 * base also lives in the preferences.
 */
class WatchService : Service() {
    @Volatile private var reader: Thread? = null
    @Volatile private var conn: HttpURLConnection? = null
    private var base: String? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        channels()
        val own = NotificationCompat.Builder(this, CHANNEL_BACKGROUND)
            .setSmallIcon(applicationInfo.icon)
            .setContentText("Watching for replies")
            .setOngoing(true)
            .build()
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(NOTE_ID, own, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        } else {
            startForeground(NOTE_ID, own)
        }
        val next = intent?.getStringExtra(PREF_BASE)
            ?: getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(PREF_BASE, null)
        if (next == null) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (next != base || reader == null) {
            halt()
            base = next
            reader = Thread({ watch(next) }, "cdash-watch").also { it.start() }
        }
        return START_STICKY
    }

    override fun onDestroy() {
        halt()
        super.onDestroy()
    }

    // ponytail: no wake lock; incoming data wakes the phone — add a partial wake lock if notes arrive late with the screen off.
    private fun watch(base: String) {
        val me = Thread.currentThread()
        var delay = FIRST_DELAY_MS
        while (reader === me) {
            var c: HttpURLConnection? = null
            try {
                c = URL("$base/api/notify").openConnection() as HttpURLConnection
                conn = c
                c.connectTimeout = 10_000
                c.readTimeout = 60_000 // the agent sends a comment every 25 s
                if (c.responseCode == 200) {
                    delay = FIRST_DELAY_MS
                    c.inputStream.bufferedReader().useLines { lines ->
                        lines.takeWhile { reader === me }.forEach { line -> dataOf(line)?.let(::post) }
                    }
                }
            } catch (_: Exception) {
            } finally {
                c?.disconnect()
            }
            try {
                Thread.sleep(delay)
            } catch (_: InterruptedException) {
                return
            }
            delay = nextDelay(delay)
        }
    }

    /** Stops the reader: a blocked read ends when its connection closes, a sleep on the interrupt. */
    private fun halt() {
        val t = reader ?: return
        reader = null
        t.interrupt()
        runCatching { conn?.disconnect() }
    }

    private fun post(json: String) {
        val o = runCatching { JSONObject(json) }.getOrNull() ?: return
        val s = o.optString("s")
        val p = o.optString("p", s)
        val k = o.optString("k")
        val text = textFor(k) ?: return
        if (s.isEmpty()) return
        val open = packageManager.getLaunchIntentForPackage(packageName)?.apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            putExtra(EXTRA_CHAT, s)
            putExtra(EXTRA_TITLE, p)
        }
        val note = NotificationCompat.Builder(this, channelFor(k))
            .setSmallIcon(applicationInfo.icon)
            .setContentTitle(p)
            .setContentText(text)
            .setAutoCancel(true)
        if (open != null) {
            // One request code per session: intents differing only in extras would otherwise share one PendingIntent.
            note.setContentIntent(
                PendingIntent.getActivity(
                    this, s.hashCode(), open, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
                )
            )
        }
        getSystemService(NotificationManager::class.java).notify(s, NOTE_ID, note.build())
    }

    private fun channels() {
        if (Build.VERSION.SDK_INT < 26) return
        getSystemService(NotificationManager::class.java).createNotificationChannels(
            listOf(
                NotificationChannel(CHANNEL_BACKGROUND, "Background", NotificationManager.IMPORTANCE_MIN),
                NotificationChannel(CHANNEL_REPLIES, "Replies", NotificationManager.IMPORTANCE_HIGH),
                NotificationChannel(CHANNEL_DONE, "Finished turns", NotificationManager.IMPORTANCE_DEFAULT),
            )
        )
    }
}

package dev.cdash.watch

import android.Manifest
import android.app.Activity
import android.app.AlertDialog
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import android.webkit.WebView
import androidx.core.content.ContextCompat
import app.tauri.PermissionState
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import org.json.JSONObject

const val PREFS = "cdash-watch"
const val PREF_BASE = "base"
const val EXTRA_CHAT = "cdash_chat"
const val EXTRA_TITLE = "cdash_title"
private const val NOTIFICATIONS = "notifications"

@InvokeArg
class StartArgs {
    lateinit var base: String
}

@InvokeArg
class ClearArgs {
    lateinit var session: String
}

/** The chat a notification tap asks for; a relaunch from Recents repeats an old tap, so it asks for none. */
fun openOf(intent: Intent?): Pair<String, String>? {
    if (intent == null || intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY != 0) return null
    val s = intent.getStringExtra(EXTRA_CHAT) ?: return null
    return s to (intent.getStringExtra(EXTRA_TITLE) ?: s)
}

@TauriPlugin(permissions = [Permission(strings = [Manifest.permission.POST_NOTIFICATIONS], alias = NOTIFICATIONS)])
class WatchPlugin(private val activity: Activity) : Plugin(activity) {
    private var webView: WebView? = null
    private var pending: Pair<String, String>? = null

    private fun prefs() = activity.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    override fun load(webView: WebView) {
        super.load(webView)
        this.webView = webView
        synchronized(this) { pending = openOf(activity.intent) }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        val (s, p) = openOf(intent) ?: return
        activity.runOnUiThread {
            webView?.evaluateJavascript(
                "window.cdashOpenChat&&window.cdashOpenChat(${JSONObject.quote(s)},${JSONObject.quote(p)})", null
            )
        }
    }

    @Command
    fun start(invoke: Invoke) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            getPermissionState(NOTIFICATIONS) != PermissionState.GRANTED
        ) {
            requestPermissionForAlias(NOTIFICATIONS, invoke, "permissionAnswered")
        } else {
            askBattery(invoke)
        }
    }

    @PermissionCallback
    private fun permissionAnswered(invoke: Invoke) {
        if (getPermissionState(NOTIFICATIONS) == PermissionState.GRANTED) askBattery(invoke)
        else invoke.reject("notifications are blocked for this app")
    }

    /** Without the exemption Android drops the connection in Doze; ask once per start, then go on either way. */
    private fun askBattery(invoke: Invoke) {
        val power = activity.getSystemService(Context.POWER_SERVICE) as PowerManager
        if (power.isIgnoringBatteryOptimizations(activity.packageName)) return begin(invoke)
        activity.runOnUiThread {
            AlertDialog.Builder(activity)
                .setTitle("Keep watching while the phone sleeps")
                .setMessage("Without this, Android cuts the connection when the phone sleeps and notifications stop.")
                .setPositiveButton("Continue") { _, _ ->
                    runCatching {
                        activity.startActivity(
                            Intent(
                                Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                                Uri.parse("package:${activity.packageName}")
                            )
                        )
                    }
                }
                .setOnDismissListener { begin(invoke) }
                .show()
        }
    }

    private fun begin(invoke: Invoke) {
        val base = invoke.parseArgs(StartArgs::class.java).base
        try {
            prefs().edit().putString(PREF_BASE, base).apply()
            ContextCompat.startForegroundService(
                activity, Intent(activity, WatchService::class.java).putExtra(PREF_BASE, base)
            )
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject(e.message ?: e.toString())
        }
    }

    @Command
    fun stop(invoke: Invoke) {
        prefs().edit().remove(PREF_BASE).apply()
        activity.stopService(Intent(activity, WatchService::class.java))
        invoke.resolve()
    }

    @Command
    fun clear(invoke: Invoke) {
        val session = invoke.parseArgs(ClearArgs::class.java).session
        (activity.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).cancel(session, NOTE_ID)
        invoke.resolve()
    }

    @Command
    fun takeOpen(invoke: Invoke) {
        val o = JSObject()
        synchronized(this) {
            pending?.let { (s, p) -> o.put("s", s); o.put("p", p) }
            pending = null
        }
        invoke.resolve(o)
    }
}

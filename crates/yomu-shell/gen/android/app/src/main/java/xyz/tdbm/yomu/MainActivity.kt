package xyz.tdbm.yomu

import android.os.Bundle
import android.view.View
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat

// Android 15+ enforces edge-to-edge for apps targeting SDK 35+, and the
// manifest opt-out is gone when targeting 36 — so not calling
// enableEdgeToEdge() is not enough: the webview still draws under the
// status/navigation bars. Pad the content view by the system-bar (and
// display-cutout) insets instead, keeping the whole UI reachable.
//
// While the reader is open ("reading") the padding stays 0 no matter
// what: the system bars overlay the page instead of resizing the
// webview, so toggling them (immersion) never shifts the reader. The
// inset heights are pushed to the page as CSS variables so the reader's
// own overlay bars can dodge the system ones.
class MainActivity : TauriActivity() {
    private var reading = false
    private var webView: WebView? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val content = findViewById<View>(android.R.id.content)
        ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
            val bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
            )
            if (reading) {
                view.setPadding(0, 0, 0, 0)
                val density = resources.displayMetrics.density
                setInsetVars(bars.top / density, bars.bottom / density)
            } else {
                view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
                setInsetVars(0f, 0f)
            }
            WindowInsetsCompat.CONSUMED
        }
    }

    private fun setInsetVars(top: Float, bottom: Float) {
        webView?.evaluateJavascript(
            "document.documentElement.style.setProperty('--shell-inset-top','${top}px');" +
                "document.documentElement.style.setProperty('--shell-inset-bottom','${bottom}px');",
            null
        )
    }

    override fun onWebViewCreate(webView: WebView) {
        this.webView = webView
        webView.addJavascriptInterface(ImmersiveBridge(), "YomuAndroid")
    }

    // Reader bridge: `setReading` marks the reader open (edge-to-edge,
    // bars overlay); `setImmersive` mirrors the chrome overlay — bars
    // hidden while reading pages, restored with the overlay.
    // The `*Updates*` methods back app-off new-chapter notifications:
    // the web UI hands over the server base URL (which schedules the
    // periodic UpdatesWorker), and the seen-watermark lives here in
    // SharedPreferences so the in-app poll and the background worker
    // share one cursor.
    // JS-interface calls arrive on a worker thread, hence runOnUiThread.
    inner class ImmersiveBridge {
        @JavascriptInterface
        fun configureUpdates(base: String) {
            val prefs = getSharedPreferences(UpdatesWorker.PREFS, MODE_PRIVATE)
            prefs.edit().putString(UpdatesWorker.KEY_BASE, base).apply()
            if (prefs.getString(UpdatesWorker.KEY_SEEN, null).isNullOrEmpty()) {
                // First run: announce from now, not the feed's backlog.
                prefs.edit().putString(UpdatesWorker.KEY_SEEN, UpdatesWorker.nowRfc3339()).apply()
            }
            UpdatesWorker.ensureChannel(this@MainActivity)
            androidx.work.WorkManager.getInstance(this@MainActivity).enqueueUniquePeriodicWork(
                "yomu-updates",
                androidx.work.ExistingPeriodicWorkPolicy.UPDATE,
                androidx.work.PeriodicWorkRequest.Builder(
                    UpdatesWorker::class.java, 30, java.util.concurrent.TimeUnit.MINUTES
                )
                    .setConstraints(
                        androidx.work.Constraints.Builder()
                            .setRequiredNetworkType(androidx.work.NetworkType.CONNECTED)
                            .build()
                    )
                    .build()
            )
        }

        @JavascriptInterface
        fun updatesWatermark(): String {
            return getSharedPreferences(UpdatesWorker.PREFS, MODE_PRIVATE)
                .getString(UpdatesWorker.KEY_SEEN, "") ?: ""
        }

        @JavascriptInterface
        fun setUpdatesWatermark(ts: String) {
            getSharedPreferences(UpdatesWorker.PREFS, MODE_PRIVATE)
                .edit().putString(UpdatesWorker.KEY_SEEN, ts).apply()
        }

        @JavascriptInterface
        fun setReading(on: Boolean) {
            runOnUiThread {
                reading = on
                ViewCompat.requestApplyInsets(findViewById(android.R.id.content))
            }
        }

        @JavascriptInterface
        fun setImmersive(on: Boolean) {
            runOnUiThread {
                val content = findViewById<View>(android.R.id.content)
                val controller = WindowCompat.getInsetsController(window, content)
                if (on) {
                    controller.systemBarsBehavior =
                        WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
                    controller.hide(WindowInsetsCompat.Type.systemBars())
                } else {
                    controller.show(WindowInsetsCompat.Type.systemBars())
                }
                ViewCompat.requestApplyInsets(content)
            }
        }
    }
}

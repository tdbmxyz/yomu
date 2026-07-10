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
    // JS-interface calls arrive on a worker thread, hence runOnUiThread.
    inner class ImmersiveBridge {
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

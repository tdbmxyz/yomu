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
// display-cutout) insets instead, keeping the whole UI reachable —
// except while the reader asks for immersion, where pages go truly
// edge-to-edge.
class MainActivity : TauriActivity() {
    private var immersive = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val content = findViewById<View>(android.R.id.content)
        ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
            if (immersive) {
                view.setPadding(0, 0, 0, 0)
            } else {
                val bars = insets.getInsets(
                    WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
                )
                view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            }
            WindowInsetsCompat.CONSUMED
        }
    }

    override fun onWebViewCreate(webView: WebView) {
        webView.addJavascriptInterface(ImmersiveBridge(), "YomuAndroid")
    }

    // Reader immersion: the UI mirrors its chrome overlay into this —
    // bars hidden while reading pages, restored with the overlay.
    // JS-interface calls arrive on a worker thread, hence runOnUiThread.
    inner class ImmersiveBridge {
        @JavascriptInterface
        fun setImmersive(on: Boolean) {
            runOnUiThread {
                immersive = on
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

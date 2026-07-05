package xyz.tdbm.yomu

import android.os.Bundle
import android.view.View
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

// Android 15+ enforces edge-to-edge for apps targeting SDK 35+, and the
// manifest opt-out is gone when targeting 36 — so not calling
// enableEdgeToEdge() is not enough: the webview still draws under the
// status/navigation bars. Pad the content view by the system-bar (and
// display-cutout) insets instead, keeping the whole UI reachable.
class MainActivity : TauriActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val content = findViewById<View>(android.R.id.content)
        ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
            val bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
            )
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            WindowInsetsCompat.CONSUMED
        }
    }
}

package xyz.tdbm.yomu

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.work.Worker
import androidx.work.WorkerParameters
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder
import org.json.JSONObject

// Polls the server's updates feed (updater-found new chapters) and posts
// one notification per manga — WorkManager runs this every ~30 min even
// with the app killed. The webview side owns the config: the `YomuAndroid`
// bridge stores the server base URL and shares the `seen` watermark with
// the in-app polling loop, so the two paths never double-announce.
class UpdatesWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result {
        val prefs = applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val base = prefs.getString(KEY_BASE, null) ?: return Result.success()
        var seen = prefs.getString(KEY_SEEN, null)
        if (seen.isNullOrEmpty()) {
            // First run: start announcing from now, not the backlog.
            prefs.edit().putString(KEY_SEEN, nowRfc3339()).apply()
            return Result.success()
        }

        val body = try {
            fetch(base.trimEnd('/') + "/api/v1/updates?since=" + URLEncoder.encode(seen, "UTF-8"))
        } catch (e: Exception) {
            return Result.retry()
        }

        try {
            val updates = JSONObject(body).getJSONArray("updates")
            for (i in 0 until updates.length()) {
                val event = updates.getJSONObject(i)
                notify(event)
                val createdAt = event.getString("created_at")
                if (createdAt > seen!!) seen = createdAt
            }
            prefs.edit().putString(KEY_SEEN, seen).apply()
        } catch (e: Exception) {
            // Unparseable response (old server?) — quietly try next round.
        }
        return Result.success()
    }

    private fun fetch(url: String): String {
        val conn = URL(url).openConnection() as HttpURLConnection
        conn.connectTimeout = 10_000
        conn.readTimeout = 10_000
        try {
            if (conn.responseCode != 200) throw RuntimeException("HTTP ${conn.responseCode}")
            return conn.inputStream.bufferedReader().readText()
        } finally {
            conn.disconnect()
        }
    }

    private fun notify(event: JSONObject) {
        if (
            ContextCompat.checkSelfPermission(
                applicationContext, Manifest.permission.POST_NOTIFICATIONS
            ) != PackageManager.PERMISSION_GRANTED
        ) {
            return // no permission: fetch still succeeded, watermark advances
        }
        val count = event.getInt("chapter_count")
        val body = if (count == 1) {
            event.getString("first_title")
        } else {
            "$count new chapters — ${event.getString("first_title")} … ${event.getString("last_title")}"
        }
        val tap = PendingIntent.getActivity(
            applicationContext,
            0,
            Intent(applicationContext, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        ensureChannel(applicationContext)
        val notification = NotificationCompat.Builder(applicationContext, CHANNEL)
            .setSmallIcon(applicationContext.applicationInfo.icon)
            .setContentTitle(event.getString("manga_title"))
            .setContentText(body)
            .setContentIntent(tap)
            .setAutoCancel(true)
            .build()
        // Tag = manga id: a later find for the same manga replaces the
        // notification instead of stacking (also dedupes vs the in-app path).
        NotificationManagerCompat.from(applicationContext)
            .notify(event.getString("manga_id"), 0, notification)
    }

    companion object {
        const val PREFS = "yomu-updates"
        const val KEY_BASE = "base"
        const val KEY_SEEN = "seen"
        const val CHANNEL = "new_chapters"

        fun ensureChannel(context: Context) {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
            val manager = context.getSystemService(NotificationManager::class.java)
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL, "New chapters", NotificationManager.IMPORTANCE_DEFAULT
                )
            )
        }

        // SimpleDateFormat, not java.time: minSdk 24 predates it.
        fun nowRfc3339(): String {
            val fmt = java.text.SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss'Z'", java.util.Locale.US)
            fmt.timeZone = java.util.TimeZone.getTimeZone("UTC")
            return fmt.format(java.util.Date())
        }
    }
}

package app.magnetar

import android.content.Context
import android.os.Build

/** Version from the manifest (AGP 9 no longer generates BuildConfig by default). */
object BuildConfigInfo {
    fun versionName(context: Context): String {
        val pm = context.packageManager
        val info = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            pm.getPackageInfo(context.packageName, android.content.pm.PackageManager.PackageInfoFlags.of(0))
        } else {
            @Suppress("DEPRECATION")
            pm.getPackageInfo(context.packageName, 0)
        }
        return info.versionName ?: "?"
    }
}

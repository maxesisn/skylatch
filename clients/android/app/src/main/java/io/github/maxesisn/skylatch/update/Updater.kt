// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.update

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.core.content.FileProvider
import io.github.maxesisn.skylatch.DownloadListener
import io.github.maxesisn.skylatch.NativeBridge
import io.github.maxesisn.skylatch.hub.DownloadedApk
import io.github.maxesisn.skylatch.hub.HubJson
import io.github.maxesisn.skylatch.hub.HubException
import io.github.maxesisn.skylatch.hub.UpdateInfo
import java.io.File

/** Self-update from GitHub Releases: check, download and verify against SHA256SUMS, then hand the APK to the installer. */
object Updater {
    /** Blocking. */
    fun check(): UpdateInfo = HubJson.update(NativeBridge.nativeCheckUpdate(null))

    /** Blocking. [progress] receives downloaded and total bytes (-1 when unknown). */
    fun download(context: Context, info: UpdateInfo, progress: (Long, Long) -> Unit): DownloadedApk {
        val url = info.downloadUrl ?: throw HubException("release has no APK")
        val sums = info.checksumsUrl ?: throw HubException("release has no SHA256SUMS")
        val asset = info.assetName ?: throw HubException("release has no APK")
        val dir = File(context.cacheDir, "updates").apply { mkdirs() }
        dir.listFiles()?.forEach { it.delete() }
        val listener = DownloadListener { done, total -> progress(done, total) }
        return HubJson.download(NativeBridge.nativeDownloadAndVerifyApk(info.version, url, sums, asset, dir.absolutePath, listener))
    }

    /** False when the user must first allow installs from this app; the settings page is opened then. */
    fun install(context: Context, apk: DownloadedApk): Boolean {
        val pm = context.packageManager
        if (!pm.canRequestPackageInstalls()) {
            context.startActivity(
                Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}"))
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            )
            return false
        }
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", File(apk.path))
        context.startActivity(
            Intent(Intent.ACTION_VIEW)
                .setDataAndType(uri, "application/vnd.android.package-archive")
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK),
        )
        return true
    }
}

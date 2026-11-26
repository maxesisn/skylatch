// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch

/**
 * Kotlin side of `libskylatch_android.so` (`clients/android/jni`).
 *
 * The HTTP, update and connect calls block until the network answers; call them off the main
 * thread. JSON replies are `{"ok":true,...}` or `{"ok":false,"error":"..."}`. Send calls return
 * [OK] or a negative status code.
 */
object NativeBridge {
    const val OK = 0
    const val ERR_HANDLE = -1
    const val ERR_CLOSED = -9
    const val ERR_QUEUE_FULL = -10
    const val ERR_NOT_READY = -11
    const val ERR_INVALID = -12
    const val ERR_UNSUPPORTED = -13

    const val TRANSPORT_UNKNOWN = 0
    const val TRANSPORT_DIRECT = 1
    const val TRANSPORT_RELAY = 2

    init {
        System.loadLibrary("skylatch_android")
    }

    external fun nativeVersion(): String

    /** `{ok, data}` with the hub's `/api/v1/info` document. */
    external fun nativeHubInfo(hubUrl: String): String

    /** `{ok, token, account}`. */
    external fun nativeLogin(hubUrl: String, username: String, password: String): String

    /** `{ok, token, account}`. */
    external fun nativeSignup(hubUrl: String, username: String, password: String): String

    /** `{ok, devices}`. */
    external fun nativeListDevices(hubUrl: String, token: String): String

    /** `{ok}`; the hub refuses (409) while the device is online. */
    external fun nativeDeleteDevice(hubUrl: String, token: String, deviceId: String): String

    /**
     * `{ok, available, version}`, plus `asset_name, size, download_url, checksums_url,
     * release_url, notes` when a newer `Skylatch-<version>-android-arm64.apk` is published.
     * A null [apiUrl] uses the GitHub Releases API.
     */
    external fun nativeCheckUpdate(apiUrl: String?): String

    /**
     * Streams the APK into [destDir] and checks it against `SHA256SUMS`. `{ok, path, sha256,
     * version}`; nothing is left behind on failure. [listener] is a [DownloadListener] or null.
     */
    external fun nativeDownloadAndVerifyApk(
        version: String,
        downloadUrl: String,
        checksumsUrl: String,
        assetName: String,
        destDir: String,
        listener: Any?,
    ): String

    /**
     * Opens a session through the hub and returns its opaque handle. On failure returns 0 after
     * calling `callback.onDisconnected(error)` on the calling thread. Callbacks run on the
     * session's own worker thread and may start before this returns.
     */
    external fun nativeConnectHub(
        hubUrl: String,
        token: String,
        deviceId: String,
        preferDirect: Boolean,
        audioEnabled: Boolean,
        callback: StreamCallback,
    ): Long

    /** As [nativeConnectHub], for a standalone host listener (`ws://` or `wss://`). */
    external fun nativeConnectDirect(
        wsUrl: String,
        token: String?,
        audioEnabled: Boolean,
        callback: StreamCallback,
    ): Long

    /** [TRANSPORT_DIRECT], [TRANSPORT_RELAY], or [TRANSPORT_UNKNOWN] for a dead handle. */
    external fun nativeTransportMode(handle: Long): Int

    external fun nativeSendAck(handle: Long, generation: Int, sequence: Int, viewerWaitUs: Int): Int

    /** One 16-byte NSL1 input record (types 1-9, including Unicode text and smooth wheel). */
    external fun nativeSendInput(handle: Long, record16: ByteArray): Int

    external fun nativeSendClipboard(handle: Long, text: String): Int

    external fun nativeSelectMonitor(handle: Long, monitorId: String): Int

    external fun nativeSetAudioEnabled(handle: Long, enabled: Boolean): Int

    /**
     * Closes the session. Idempotent; once it returns no callback for this handle runs again.
     * May block briefly for the close handshake. Callbacks must not wait on the thread that
     * calls this; calling it from inside a callback is allowed.
     */
    external fun nativeDestroy(handle: Long)
}

/** `generation`, `sequence` and `sampleRate` carry unsigned 32-bit values. */
interface StreamCallback {
    fun onVideoFrame(
        generation: Int,
        sequence: Int,
        width: Int,
        height: Int,
        captureUs: Long,
        isKeyframe: Boolean,
        annexB: ByteArray,
    )

    /** A null [rgba] (with zero sizes) restores the local pointer. */
    fun onCursor(width: Int, height: Int, xhot: Int, yhot: Int, rgba: ByteArray?)

    /** Interleaved 16-bit samples. */
    fun onAudioPcm(captureUs: Long, sampleRate: Int, channels: Int, pcm: ShortArray)

    fun onMonitorList(json: String)

    fun onClipboardData(text: String)

    fun onInputResponse(sequence: Int, status: Int)

    /** Last callback of a session; [error] is null for a clean close. */
    fun onDisconnected(error: String?)
}

fun interface DownloadListener {
    /** [totalBytes] is -1 when the server sends no length. */
    fun onProgress(downloadedBytes: Long, totalBytes: Long)
}

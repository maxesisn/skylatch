// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.stream

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.Bitmap
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import android.view.Surface
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import io.github.maxesisn.skylatch.NativeBridge
import io.github.maxesisn.skylatch.StreamCallback
import io.github.maxesisn.skylatch.hub.Device
import io.github.maxesisn.skylatch.hub.HubJson
import io.github.maxesisn.skylatch.hub.Monitor
import io.github.maxesisn.skylatch.input.ScrollEncoder
import java.util.concurrent.Executor

/** The host's pointer image; [visible] is false for a fully transparent cursor. */
class RemoteCursor(val bitmap: Bitmap, val hotX: Int, val hotY: Int, val visible: Boolean)

/**
 * One streaming connection to a device: owns the decoder, the audio output and the native
 * session handle, and publishes what the stream page shows as Compose state.
 */
class StreamSession(
    context: Context,
    val device: Device,
    private val hubUrl: String,
    private val token: String,
    private val preferDirect: Boolean,
    audioEnabled: Boolean,
    private val io: Executor,
) {
    enum class Phase { CONNECTING, LIVE, CLOSED }

    var phase by mutableStateOf(Phase.CONNECTING)
        private set
    var error by mutableStateOf<String?>(null)
        private set
    var videoWidth by mutableIntStateOf(0)
        private set
    var videoHeight by mutableIntStateOf(0)
        private set
    var firstFrame by mutableStateOf(false)
        private set
    var cursor by mutableStateOf<RemoteCursor?>(null)
        private set
    var monitors by mutableStateOf<List<Monitor>>(emptyList())
        private set
    var audioOn by mutableStateOf(audioEnabled)
        private set

    val stats = PerfStats()
    val scroll = ScrollEncoder()

    private val clipboard = context.getSystemService(ClipboardManager::class.java)
    private val main = Handler(Looper.getMainLooper())
    private val ackLock = Any()
    private val pendingAcks = ArrayList<IntArray>()

    @Volatile
    private var handle = 0L

    @Volatile
    private var attempt: Attempt? = null
    private var decoder: VideoDecoder? = null
    private var audio: AudioOut? = null
    private var surface: Surface? = null
    private var lastRemoteClip: String? = null
    private var lastSentClip: String? = null
    private var reconnects = 0

    fun connect() {
        teardown()
        phase = Phase.CONNECTING
        error = null
        firstFrame = false
        cursor = null
        stats.reset()
        scroll.reset()
        val a = Attempt()
        attempt = a
        decoder = VideoDecoder(stats, { g, s, w -> if (attempt === a) sendAck(g, s, w) }) { main.post { if (attempt === a) firstFrame = true } }
            .also { it.setSurface(surface) }
        audio = AudioOut()
        val wantAudio = audioOn
        io.execute {
            val h = NativeBridge.nativeConnectHub(hubUrl, token, device.id, preferDirect, wantAudio, a)
            if (h == 0L) return@execute
            if (attempt !== a) {
                NativeBridge.nativeDestroy(h)
                return@execute
            }
            synchronized(ackLock) {
                handle = h
                for (p in pendingAcks) NativeBridge.nativeSendAck(h, p[0], p[1], p[2])
                pendingAcks.clear()
            }
            main.post { if (attempt === a && phase == Phase.CONNECTING) phase = Phase.LIVE }
        }
    }

    fun close() {
        teardown()
        phase = Phase.CLOSED
    }

    fun setSurface(target: Surface?) {
        surface = target
        decoder?.setSurface(target)
    }

    fun send(record: ByteArray): Int {
        val h = handle
        return if (h == 0L) NativeBridge.ERR_NOT_READY else NativeBridge.nativeSendInput(h, record)
    }

    fun transport(): PerfPath = when (if (handle == 0L) 0 else NativeBridge.nativeTransportMode(handle)) {
        NativeBridge.TRANSPORT_DIRECT -> PerfPath.DIRECT
        NativeBridge.TRANSPORT_RELAY -> PerfPath.RELAY
        else -> PerfPath.UNKNOWN
    }

    fun readout(): PerfReadout = stats.readout(transport(), if (audioOn) PerfAudio.ON else PerfAudio.MUTED)

    fun setAudio(on: Boolean) {
        audioOn = on
        if (!on) audio?.clear()
        val h = handle
        if (h != 0L) NativeBridge.nativeSetAudioEnabled(h, on)
    }

    fun selectMonitor(id: String) {
        val h = handle
        if (h != 0L) NativeBridge.nativeSelectMonitor(h, id)
    }

    /** Sends the local clipboard text. Unless [force], text that came from the host or was already sent is skipped. */
    fun pushClipboard(force: Boolean): Boolean {
        val h = handle
        if (h == 0L) return false
        val text = runCatching { clipboard?.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.text?.toString() }.getOrNull()
        if (text.isNullOrEmpty()) return false
        if (!force && (text == lastRemoteClip || text == lastSentClip)) return false
        lastSentClip = text
        return NativeBridge.nativeSendClipboard(h, text) == NativeBridge.OK
    }

    /** Reconnects once a stream lost its decoder and the host sent no keyframe to restart it. */
    fun tick() {
        val starved = decoder?.starvedSinceNs ?: 0L
        if (phase == Phase.LIVE && firstFrame && starved != 0L && surface != null &&
            SystemClock.elapsedRealtimeNanos() - starved > 2_000_000_000L && reconnects < 3
        ) {
            Log.i(TAG, "no keyframe after decoder loss; reconnecting")
            reconnects++
            connect()
        }
    }

    private fun sendAck(generation: Int, sequence: Int, waitUs: Int) {
        synchronized(ackLock) {
            val h = handle
            if (h == 0L) pendingAcks += intArrayOf(generation, sequence, waitUs) else NativeBridge.nativeSendAck(h, generation, sequence, waitUs)
        }
    }

    private fun teardown() {
        attempt = null
        val h: Long
        synchronized(ackLock) {
            h = handle
            handle = 0L
            pendingAcks.clear()
        }
        decoder?.release()
        decoder = null
        audio?.let { a -> io.execute { a.release() } }
        audio = null
        if (h != 0L) io.execute { NativeBridge.nativeDestroy(h) }
    }

    private inner class Attempt : StreamCallback {
        private var lastW = 0
        private var lastH = 0

        private fun current() = attempt === this

        override fun onVideoFrame(generation: Int, sequence: Int, width: Int, height: Int, captureUs: Long, isKeyframe: Boolean, annexB: ByteArray) {
            val d = decoder
            if (!current() || d == null) return
            d.submit(generation, sequence, width, height, isKeyframe, annexB)
            if (width != lastW || height != lastH) {
                lastW = width
                lastH = height
                main.post {
                    if (current()) {
                        videoWidth = width
                        videoHeight = height
                    }
                }
            }
        }

        override fun onCursor(width: Int, height: Int, xhot: Int, yhot: Int, rgba: ByteArray?) {
            if (!current()) return
            val image = if (rgba == null || width <= 0 || height <= 0 || rgba.size < width * height * 4) {
                null
            } else {
                val px = IntArray(width * height)
                var any = false
                for (i in px.indices) {
                    val a = rgba[i * 4 + 3].toInt() and 0xff
                    if (a != 0) any = true
                    px[i] = (a shl 24) or ((rgba[i * 4].toInt() and 0xff) shl 16) or
                        ((rgba[i * 4 + 1].toInt() and 0xff) shl 8) or (rgba[i * 4 + 2].toInt() and 0xff)
                }
                RemoteCursor(Bitmap.createBitmap(px, width, height, Bitmap.Config.ARGB_8888), xhot, yhot, any)
            }
            main.post { if (current()) cursor = image }
        }

        override fun onAudioPcm(captureUs: Long, sampleRate: Int, channels: Int, pcm: ShortArray) {
            val a = audio
            if (!current() || a == null || !audioOn) return
            a.push(pcm, sampleRate, channels)
            stats.audioBuffered(a.bufferedMs)
        }

        override fun onMonitorList(json: String) {
            val list = HubJson.monitors(json)
            main.post { if (current()) monitors = list }
        }

        override fun onClipboardData(text: String) {
            main.post {
                if (!current()) return@post
                lastRemoteClip = text
                val local = runCatching { clipboard?.primaryClip?.getItemAt(0)?.text?.toString() }.getOrNull()
                if (local != text) runCatching { clipboard?.setPrimaryClip(ClipData.newPlainText("Skylatch", text)) }
            }
        }

        override fun onInputResponse(sequence: Int, status: Int) {
            if (current()) scroll.onInputResponse(sequence, status)
        }

        override fun onDisconnected(error: String?) {
            main.post {
                if (!current()) return@post
                teardown()
                this@StreamSession.error = error ?: "主机结束了会话"
                phase = Phase.CLOSED
            }
        }
    }

    companion object {
        private const val TAG = "SkylatchStream"
    }
}

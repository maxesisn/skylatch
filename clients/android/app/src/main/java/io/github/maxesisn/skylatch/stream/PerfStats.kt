// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.stream

import java.util.ArrayDeque
import java.util.Locale

/** Averages over the last second of a session. Null means nothing in the window. */
data class PerfRates(
    val fps: Double = 0.0,
    val mbps: Double = 0.0,
    val decodeMs: Double? = null,
    val waitMs: Double? = null,
    val audioMs: Double? = null,
)

/** Sliding window of per-frame and per-audio-packet measurements; times are monotonic nanoseconds. */
class PerfWindow(private val spanNs: Long = 1_000_000_000L) {
    private class Sample(val at: Long, val value: Double)

    private val received = ArrayDeque<Sample>()
    private val decoded = ArrayDeque<Sample>()
    private val presented = ArrayDeque<Sample>()
    private val audio = ArrayDeque<Sample>()

    init {
        require(spanNs > 0)
    }

    fun received(bytes: Int, at: Long) = add(received, at, bytes.toDouble())

    fun decoded(decodeUs: Long, at: Long) = add(decoded, at, decodeUs.toDouble())

    fun presented(waitUs: Long, at: Long) = add(presented, at, waitUs.toDouble())

    fun audioBuffered(ms: Double, at: Long) = add(audio, at, ms)

    fun reset() {
        received.clear(); decoded.clear(); presented.clear(); audio.clear()
    }

    fun rates(now: Long): PerfRates {
        prune(now)
        val seconds = spanNs / 1e9
        return PerfRates(
            fps = presented.size / seconds,
            mbps = received.sumOf { it.value } * 8 / 1e6 / seconds,
            decodeMs = mean(decoded)?.div(1000),
            waitMs = mean(presented)?.div(1000),
            audioMs = mean(audio),
        )
    }

    private fun mean(list: ArrayDeque<Sample>): Double? =
        if (list.isEmpty()) null else list.sumOf { it.value } / list.size

    private fun add(list: ArrayDeque<Sample>, at: Long, value: Double) {
        list.addLast(Sample(at, value))
        prune(at)
    }

    private fun prune(now: Long) {
        val cutoff = now - spanNs
        for (list in arrayOf(received, decoded, presented, audio)) {
            while (list.isNotEmpty() && list.peekFirst()!!.at <= cutoff) list.removeFirst()
        }
    }
}

enum class HudMode {
    HIDDEN, CARD, STRIP;

    val next: HudMode get() = entries[(ordinal + 1) % entries.size]
}

enum class PerfPath { DIRECT, RELAY, UNKNOWN }

enum class PerfAudio { OFF, MUTED, ON }

data class PerfReadout(
    val width: Int = 0,
    val height: Int = 0,
    /** `HW` or `SW`, empty before the decoder exists. */
    val decoder: String = "",
    val path: PerfPath = PerfPath.UNKNOWN,
    val audio: PerfAudio = PerfAudio.OFF,
    val rates: PerfRates = PerfRates(),
) {
    val isSlow: Boolean get() = rates.fps < 30
}

data class PerfRow(val left: String, val right: String)

object PerfFormat {
    private const val GAP = "\u2003"

    fun ms(v: Double?): String = if (v == null) "--" else String.format(Locale.ROOT, "%.1fms", maxOf(0.0, v))

    fun mbps(v: Double): String =
        String.format(Locale.ROOT, if (v >= 100) "%.0f Mbps" else "%.1f Mbps", maxOf(0.0, v))

    fun fps(v: Double): String = String.format(Locale.ROOT, "%.0f FPS", maxOf(0.0, v))

    fun resolution(r: PerfReadout): String = if (r.width > 0 && r.height > 0) "${r.width}\u00D7${r.height}" else ""

    fun path(r: PerfReadout): String = when (r.path) {
        PerfPath.DIRECT -> "直连"
        PerfPath.RELAY -> "中继"
        PerfPath.UNKNOWN -> ""
    }

    fun audio(r: PerfReadout): String = when (r.audio) {
        PerfAudio.OFF -> "关闭"
        PerfAudio.MUTED -> "已静音"
        PerfAudio.ON -> r.rates.audioMs?.let { String.format(Locale.ROOT, "Opus %.0fms", maxOf(0.0, it)) } ?: "无数据"
    }

    private fun decoder(r: PerfReadout): String =
        listOf(r.decoder, ms(r.rates.decodeMs)).filter { it.isNotEmpty() }.joinToString(" ")

    private fun join(parts: List<String>): String = parts.filter { it.isNotEmpty() }.joinToString(GAP)

    /** Three rows: FPS and resolution with the path, decoder with the viewer wait, bitrate with audio. */
    fun card(r: PerfReadout): List<PerfRow> = listOf(
        PerfRow(join(listOf(fps(r.rates.fps), resolution(r))), path(r)),
        PerfRow("解 ${decoder(r)}", "等待 ${ms(r.rates.waitMs)}"),
        PerfRow("码率 ${mbps(r.rates.mbps)}", "音频 ${audio(r)}"),
    )

    fun strip(r: PerfReadout): String =
        join(listOf(fps(r.rates.fps), resolution(r), decoder(r), mbps(r.rates.mbps), path(r)))
}

/** Where the HUD may sit: wholly inside the bounds, measured from the top-left corner. */
object HudPlacement {
    fun clamp(x: Float, y: Float, width: Float, height: Float, boundsWidth: Float, boundsHeight: Float): Pair<Float, Float> =
        x.coerceIn(0f, maxOf(0f, boundsWidth - width)) to y.coerceIn(0f, maxOf(0f, boundsHeight - height))
}

/** Thread-safe session statistics shared by the decoder, the audio path and the HUD. */
class PerfStats(private val clock: () -> Long = System::nanoTime) {
    private val window = PerfWindow()

    @Volatile
    var decoder: String = ""

    @Volatile
    var width: Int = 0

    @Volatile
    var height: Int = 0

    fun received(bytes: Int) = synchronized(window) { window.received(bytes, clock()) }

    fun decoded(decodeUs: Long) = synchronized(window) { window.decoded(decodeUs, clock()) }

    fun presented(waitUs: Long) = synchronized(window) { window.presented(waitUs, clock()) }

    fun audioBuffered(ms: Double) = synchronized(window) { window.audioBuffered(ms, clock()) }

    fun reset() = synchronized(window) { window.reset() }

    fun readout(path: PerfPath, audio: PerfAudio): PerfReadout {
        val rates = synchronized(window) { window.rates(clock()) }
        return PerfReadout(width, height, decoder, path, audio, rates)
    }
}

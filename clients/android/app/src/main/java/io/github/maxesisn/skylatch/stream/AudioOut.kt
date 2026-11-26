// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.stream

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.util.Log

/**
 * Jitter buffer of interleaved 16-bit samples. Playback starts once [startSamples] are queued and
 * waits for that much again after an underrun; a backlog beyond [maxSamples] drops the oldest
 * samples back down to [startSamples], so latency never snowballs.
 */
class PcmRing(capacity: Int, val startSamples: Int, val maxSamples: Int) {
    private val buf = ShortArray(capacity)
    private var head = 0
    var size = 0
        private set
    var primed = false
        private set
    var dropped = 0L
        private set

    init {
        require(startSamples in 1..maxSamples && maxSamples <= capacity)
    }

    fun write(src: ShortArray, off: Int = 0, len: Int = src.size - off) {
        var from = off
        var n = len
        if (n > buf.size) {
            dropped += n - buf.size
            from += n - buf.size
            n = buf.size
        }
        if (size + n > buf.size) drop(size + n - buf.size)
        for (i in 0 until n) buf[(head + size + i) % buf.size] = src[from + i]
        size += n
        if (size > maxSamples) drop(size - startSamples)
    }

    /** Copies up to [len] samples into [dst]; nothing while the buffer is still filling up. */
    fun read(dst: ShortArray, len: Int = dst.size): Int {
        if (!primed) {
            if (size < startSamples) return 0
            primed = true
        }
        val n = minOf(len, size)
        for (i in 0 until n) dst[i] = buf[(head + i) % buf.size]
        head = (head + n) % buf.size
        size -= n
        if (size == 0) primed = false
        return n
    }

    fun clear() {
        head = 0
        size = 0
        primed = false
    }

    private fun drop(n: Int) {
        val d = minOf(n, size)
        head = (head + d) % buf.size
        size -= d
        dropped += d
    }
}

/** Low-latency playback of the host's decoded 48 kHz stereo audio. */
class AudioOut {
    private val lock = Object()
    private var ring: PcmRing? = null
    private var track: AudioTrack? = null
    private var thread: Thread? = null
    private var rate = 0
    private var channels = 0
    private var framesWritten = 0L

    @Volatile
    private var running = true

    /** Queued audio plus what the track has not played yet, in milliseconds. */
    val bufferedMs: Double
        get() = synchronized(lock) {
            val r = ring ?: return 0.0
            val t = track
            val pending = if (t != null) (framesWritten - (t.playbackHeadPosition.toLong() and 0xffffffffL)).coerceAtLeast(0) else 0
            (r.size / channels + pending) * 1000.0 / rate
        }

    fun push(pcm: ShortArray, sampleRate: Int, channelCount: Int) {
        if (!running || sampleRate <= 0 || channelCount !in 1..2) return
        synchronized(lock) {
            if (sampleRate != rate || channelCount != channels || track == null) open(sampleRate, channelCount)
            ring?.write(pcm)
            lock.notifyAll()
        }
    }

    fun clear() = synchronized(lock) {
        ring?.clear()
        track?.let { runCatching { it.pause(); it.flush(); it.play() } }
        framesWritten = track?.playbackHeadPosition?.toLong()?.and(0xffffffffL) ?: 0
    }

    fun release() {
        running = false
        synchronized(lock) { lock.notifyAll() }
        thread?.join(500)
        synchronized(lock) {
            track?.let { runCatching { it.stop() }; it.release() }
            track = null
            ring = null
        }
    }

    private fun open(sampleRate: Int, channelCount: Int) {
        track?.let { runCatching { it.stop() }; it.release() }
        rate = sampleRate
        channels = channelCount
        val perMs = sampleRate * channelCount / 1000
        ring = PcmRing(capacity = perMs * 200, startSamples = perMs * START_MS, maxSamples = perMs * MAX_MS)
        val mask = if (channelCount == 2) AudioFormat.CHANNEL_OUT_STEREO else AudioFormat.CHANNEL_OUT_MONO
        val min = AudioTrack.getMinBufferSize(sampleRate, mask, AudioFormat.ENCODING_PCM_16BIT)
        track = runCatching {
            AudioTrack.Builder()
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_UNKNOWN)
                        .build(),
                )
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setSampleRate(sampleRate)
                        .setChannelMask(mask)
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .build(),
                )
                .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .setBufferSizeInBytes(maxOf(min, perMs * 2 * 20))
                .build()
                .also {
                    it.setBufferSizeInFrames(sampleRate * TRACK_MS / 1000)
                    it.play()
                }
        }.onFailure { Log.w(TAG, "AudioTrack unavailable", it) }.getOrNull()
        framesWritten = 0
        if (thread == null) {
            thread = Thread(::pump, "skylatch-audio").apply {
                priority = Thread.MAX_PRIORITY
                start()
            }
        }
    }

    private fun pump() {
        android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_AUDIO)
        val chunk = ShortArray(48 * 2 * CHUNK_MS * 2)
        while (running) {
            var target: AudioTrack? = null
            var n = 0
            synchronized(lock) {
                while (running && target == null) {
                    val r = ring
                    val t = track
                    if (r != null && t != null) {
                        n = r.read(chunk, minOf(chunk.size, rate * channels * CHUNK_MS / 1000))
                        if (n > 0) target = t
                    }
                    if (target == null) lock.wait(20)
                }
            }
            val t = target ?: continue
            val written = runCatching { t.write(chunk, 0, n, AudioTrack.WRITE_BLOCKING) }.getOrDefault(0)
            if (written > 0) synchronized(lock) { if (track === t) framesWritten += written / channels }
        }
    }

    companion object {
        private const val TAG = "SkylatchAudio"
        const val START_MS = 40
        const val MAX_MS = 120
        const val CHUNK_MS = 5
        const val TRACK_MS = 20
    }
}

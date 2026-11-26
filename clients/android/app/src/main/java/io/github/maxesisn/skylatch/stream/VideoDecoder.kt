// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.stream

import android.graphics.ImageFormat
import android.media.ImageReader
import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import android.view.Surface
import java.util.ArrayDeque
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * Hardware H.264 decoding straight into the video [Surface]. Every frame is acknowledged exactly
 * once, in order: when it is rendered, or when the decoder skips it or is rebuilt. All codec work
 * runs on one handler thread, which also receives the codec's callbacks.
 */
class VideoDecoder(
    private val stats: PerfStats,
    private val ack: (generation: Int, sequence: Int, waitUs: Int) -> Unit,
    private val onFirstFrame: () -> Unit,
) {
    private class Pending(
        val generation: Int,
        val sequence: Int,
        val keyframe: Boolean,
        val data: ByteArray,
        val arrivalNs: Long,
    ) {
        var pts = 0L
        var queuedNs = 0L
    }

    private val thread = HandlerThread("skylatch-video").apply { start() }
    private val handler = Handler(thread.looper)

    private var codec: MediaCodec? = null
    private var params: AnnexB.ParameterSets? = null
    private var width = 0
    private var height = 0
    private var surface: Surface? = null
    private var sink: ImageReader? = null
    private val freeInputs = ArrayDeque<Int>()
    private val waiting = ArrayDeque<Pending>()
    private val inflight = ArrayDeque<Pending>()
    private var nextPts = 1L
    private var rendered = false
    private var released = false

    /** Nanosecond uptime since the decoder has been waiting for a keyframe it cannot decode without, or 0. */
    @Volatile
    var starvedSinceNs = 0L
        private set

    fun submit(generation: Int, sequence: Int, width: Int, height: Int, keyframe: Boolean, data: ByteArray) {
        val frame = Pending(generation, sequence, keyframe, data, SystemClock.elapsedRealtimeNanos())
        stats.received(data.size)
        handler.post { onFrame(frame, width, height) }
    }

    /** The on-screen surface, or null while there is none; decoding then continues into an offscreen sink. */
    fun setSurface(target: Surface?) {
        val done = CountDownLatch(1)
        val posted = handler.post {
            try {
                switchSurface(target)
            } finally {
                done.countDown()
            }
        }
        if (posted && Looper.myLooper() != thread.looper) done.await(1, TimeUnit.SECONDS)
    }

    private fun switchSurface(target: Surface?) {
        surface = target
        val c = codec ?: return
        val out = target ?: sinkSurface()
        try {
            if (out == null) throw IllegalStateException("no output surface")
            c.setOutputSurface(out)
        } catch (e: RuntimeException) {
            Log.w(TAG, "cannot switch the output surface; waiting for the next keyframe", e)
            teardown()
            starvedSinceNs = SystemClock.elapsedRealtimeNanos()
        }
    }

    fun release() {
        handler.post {
            released = true
            teardown()
            sink?.close()
            sink = null
            thread.quitSafely()
        }
    }

    private fun onFrame(frame: Pending, w: Int, h: Int) {
        if (released) return drop(frame)
        if (frame.keyframe) {
            val sets = AnnexB.parameterSets(frame.data)
            if (codec == null || w != width || h != height || (sets != null && sets != params)) {
                rebuild(w, h, sets)
            }
        }
        if (codec == null) {
            if (starvedSinceNs == 0L) starvedSinceNs = SystemClock.elapsedRealtimeNanos()
            return drop(frame)
        }
        waiting.addLast(frame)
        feed()
    }

    private fun drop(frame: Pending) {
        ack(frame.generation, frame.sequence, waitUs(frame, SystemClock.elapsedRealtimeNanos()))
    }

    private fun waitUs(frame: Pending, now: Long): Int = ((now - frame.arrivalNs) / 1000).coerceIn(0, Int.MAX_VALUE.toLong()).toInt()

    private fun feed() {
        val c = codec ?: return
        while (freeInputs.isNotEmpty() && waiting.isNotEmpty()) {
            val index = freeInputs.removeFirst()
            val frame = waiting.removeFirst()
            try {
                val buf = c.getInputBuffer(index) ?: throw IllegalStateException("no input buffer")
                if (buf.capacity() < frame.data.size) throw IllegalStateException("access unit larger than the input buffer")
                buf.clear()
                buf.put(frame.data)
                frame.pts = nextPts++
                frame.queuedNs = SystemClock.elapsedRealtimeNanos()
                inflight.addLast(frame)
                c.queueInputBuffer(index, 0, frame.data.size, frame.pts, if (frame.keyframe) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0)
            } catch (e: RuntimeException) {
                Log.w(TAG, "queueInputBuffer failed", e)
                drop(frame)
                fail()
                return
            }
        }
    }

    private fun rebuild(w: Int, h: Int, sets: AnnexB.ParameterSets?) {
        teardown()
        width = w
        height = h
        params = sets
        val out = surface ?: sinkSurface() ?: return
        val c = try {
            MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
        } catch (e: Exception) {
            Log.e(TAG, "no H.264 decoder", e)
            return
        }
        if (!configure(c, out, lowLatency = true) && !configure(c, out, lowLatency = false)) {
            c.release()
            return
        }
        try {
            c.start()
        } catch (e: RuntimeException) {
            Log.e(TAG, "decoder start failed", e)
            c.release()
            return
        }
        codec = c
        starvedSinceNs = 0L
        stats.decoder = if (isHardware(c)) "HW" else "SW"
        stats.width = w
        stats.height = h
        Log.i(TAG, "decoder ${c.name} ${w}x$h (${stats.decoder})")
    }

    private fun configure(c: MediaCodec, out: Surface, lowLatency: Boolean): Boolean {
        val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height)
        params?.let {
            format.setByteBuffer("csd-0", java.nio.ByteBuffer.wrap(it.sps))
            format.setByteBuffer("csd-1", java.nio.ByteBuffer.wrap(it.pps))
        }
        format.setInteger(MediaFormat.KEY_PRIORITY, 0)
        if (lowLatency) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            format.setInteger("vendor.qti-ext-dec-low-latency.enable", 1)
            format.setInteger("vendor.qti-ext-dec-picture-order.enable", 1)
        }
        return try {
            c.setCallback(callback, handler)
            c.configure(format, out, null, 0)
            true
        } catch (e: RuntimeException) {
            Log.w(TAG, "configure (lowLatency=$lowLatency) failed", e)
            runCatching { c.reset() }
            false
        }
    }

    private fun isHardware(c: MediaCodec): Boolean {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) return c.codecInfo.isHardwareAccelerated
        val name = c.name.lowercase()
        return !name.startsWith("omx.google.") && !name.startsWith("c2.android.")
    }

    private fun sinkSurface(): Surface? {
        if (width <= 0 || height <= 0) return null
        val current = sink
        if (current != null && current.width == width && current.height == height) return current.surface
        current?.close()
        return try {
            ImageReader.newInstance(width, height, ImageFormat.PRIVATE, 3).also { reader ->
                reader.setOnImageAvailableListener({ r -> r.acquireLatestImage()?.close() }, handler)
                sink = reader
            }.surface
        } catch (e: RuntimeException) {
            Log.w(TAG, "no offscreen sink", e)
            null
        }
    }

    private fun teardown() {
        val c = codec
        codec = null
        if (c != null) {
            runCatching { c.stop() }
            c.release()
        }
        freeInputs.clear()
        val now = SystemClock.elapsedRealtimeNanos()
        while (inflight.isNotEmpty()) inflight.removeFirst().let { ack(it.generation, it.sequence, waitUs(it, now)) }
        while (waiting.isNotEmpty()) waiting.removeFirst().let { ack(it.generation, it.sequence, waitUs(it, now)) }
    }

    private fun fail() {
        teardown()
        params = null
        starvedSinceNs = SystemClock.elapsedRealtimeNanos()
    }

    private val callback = object : MediaCodec.Callback() {
        override fun onInputBufferAvailable(c: MediaCodec, index: Int) {
            if (c !== codec) return
            freeInputs.addLast(index)
            feed()
        }

        override fun onOutputBufferAvailable(c: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
            if (c !== codec) return
            val config = info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0
            try {
                c.releaseOutputBuffer(index, !config && info.size > 0)
            } catch (e: RuntimeException) {
                Log.w(TAG, "releaseOutputBuffer failed", e)
                fail()
                return
            }
            if (config) return
            val now = SystemClock.elapsedRealtimeNanos()
            while (inflight.isNotEmpty() && inflight.peekFirst()!!.pts < info.presentationTimeUs) {
                inflight.removeFirst().let { ack(it.generation, it.sequence, waitUs(it, now)) }
            }
            val frame = if (inflight.peekFirst()?.pts == info.presentationTimeUs) inflight.removeFirst() else return
            val wait = waitUs(frame, now)
            ack(frame.generation, frame.sequence, wait)
            stats.decoded((now - frame.queuedNs) / 1000)
            stats.presented(wait.toLong())
            if (!rendered) {
                rendered = true
                onFirstFrame()
            }
        }

        override fun onError(c: MediaCodec, e: MediaCodec.CodecException) {
            if (c !== codec) return
            Log.e(TAG, "decoder error", e)
            fail()
        }

        override fun onOutputFormatChanged(c: MediaCodec, format: MediaFormat) {
            Log.i(TAG, "output format $format")
        }
    }

    companion object {
        private const val TAG = "SkylatchVideo"
    }
}

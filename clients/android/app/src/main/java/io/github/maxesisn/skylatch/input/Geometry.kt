// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.input

import kotlin.math.floor
import kotlin.math.hypot

/** An axis-aligned rectangle in screen pixels. */
data class Box(val left: Float, val top: Float, val width: Float, val height: Float) {
    val right: Float get() = left + width
    val bottom: Float get() = top + height
    val isEmpty: Boolean get() = width <= 0f || height <= 0f

    fun contains(x: Float, y: Float): Boolean = x >= left && x < right && y >= top && y < bottom

    companion object {
        val EMPTY = Box(0f, 0f, 0f, 0f)
    }
}

/** Pinch zoom of the video: [scale] >= 1 and the translation of the scaled rectangle from the fitted one. */
data class Zoom(val scale: Float = 1f, val tx: Float = 0f, val ty: Float = 0f)

object Geometry {
    const val MAX_ZOOM = 4f

    /** Largest rectangle with the video's aspect ratio that fits `width` x `height`, centred. */
    fun letterbox(width: Float, height: Float, videoWidth: Int, videoHeight: Int): Box {
        if (videoWidth <= 0 || videoHeight <= 0 || width <= 0f || height <= 0f) return Box.EMPTY
        var w = width
        var h = height
        if (width * videoHeight > height * videoWidth) {
            w = maxOf(1f, floor(height * videoWidth / videoHeight))
        } else {
            h = maxOf(1f, floor(width * videoHeight / videoWidth))
        }
        return Box(floor((width - w) / 2), floor((height - h) / 2), w, h)
    }

    /** Maps a coordinate in `[start, start + size)` to `0..65535`, clamping to the range. */
    fun normalize(p: Float, start: Float, size: Float): Int {
        if (size <= 1f) return 0
        val clamped = p.coerceIn(start, start + size - 1f)
        return minOf(65535, floor((clamped - start) * 65535.0 / (size - 1f)).toInt())
    }

    /** The on-screen position of a normalized coordinate. */
    fun denormalize(v: Int, start: Float, size: Float): Float = start + v / 65535f * (size - 1f).coerceAtLeast(0f)

    /** Keeps the zoomed rectangle covering the fitted one, so no black edge opens inside it. */
    fun clampZoom(base: Box, zoom: Zoom): Zoom {
        val scale = zoom.scale.coerceIn(1f, MAX_ZOOM)
        val minX = base.width * (1f - scale)
        val minY = base.height * (1f - scale)
        return Zoom(scale, zoom.tx.coerceIn(minX, 0f), zoom.ty.coerceIn(minY, 0f))
    }

    fun zoomed(base: Box, zoom: Zoom): Box {
        val z = clampZoom(base, zoom)
        return Box(base.left + z.tx, base.top + z.ty, base.width * z.scale, base.height * z.scale)
    }

    /**
     * Zoom after a pinch step: the content under [startFocus] when the gesture began stays under
     * [focus], scaled by [spanRatio] from the gesture's starting zoom.
     */
    fun pinch(base: Box, start: Zoom, startFocusX: Float, startFocusY: Float, focusX: Float, focusY: Float, spanRatio: Float): Zoom {
        val scale = (start.scale * spanRatio).coerceIn(1f, MAX_ZOOM)
        val qx = (startFocusX - base.left - start.tx) / start.scale
        val qy = (startFocusY - base.top - start.ty) / start.scale
        return clampZoom(base, Zoom(scale, focusX - base.left - qx * scale, focusY - base.top - qy * scale))
    }

    /** Pans a zoomed view just enough to keep a point (screen pixels) inside the fitted rectangle with [margin]. */
    fun follow(base: Box, zoom: Zoom, x: Float, y: Float, margin: Float): Zoom {
        if (zoom.scale <= 1f) return zoom
        var tx = zoom.tx
        var ty = zoom.ty
        if (x < base.left + margin) tx += base.left + margin - x
        if (x > base.right - margin) tx -= x - (base.right - margin)
        if (y < base.top + margin) ty += base.top + margin - y
        if (y > base.bottom - margin) ty -= y - (base.bottom - margin)
        return clampZoom(base, Zoom(zoom.scale, tx, ty))
    }
}

/**
 * Relative pointer for trackpad mode. The position is kept in wire units (`0..65535` on each
 * axis) as a double, so slow finger movement still accumulates.
 */
class Trackpad {
    var x = 32767.5
        private set
    var y = 32767.5
        private set

    val wireX: Int get() = x.toInt().coerceIn(0, 65535)
    val wireY: Int get() = y.toInt().coerceIn(0, 65535)

    fun set(wireX: Int, wireY: Int) {
        x = wireX.toDouble().coerceIn(0.0, 65535.0)
        y = wireY.toDouble().coerceIn(0.0, 65535.0)
    }

    /**
     * Moves by a finger delta over [dtMs]. [view] is the video rectangle on screen; a finger moving
     * across it once at slow speed moves the pointer across the desktop once, faster swipes go further.
     */
    fun move(dx: Float, dy: Float, dtMs: Float, view: Box): Boolean {
        if (view.isEmpty || (dx == 0f && dy == 0f)) return false
        val g = gain(hypot(dx, dy) / dtMs.coerceAtLeast(1f))
        val oldX = wireX
        val oldY = wireY
        x = (x + dx * g * 65535.0 / view.width).coerceIn(0.0, 65535.0)
        y = (y + dy * g * 65535.0 / view.height).coerceIn(0.0, 65535.0)
        return wireX != oldX || wireY != oldY
    }

    companion object {
        const val BASE_GAIN = 0.9
        const val ACCEL = 0.85
        const val MAX_GAIN = 3.2

        /** Gain for a finger speed in pixels per millisecond. */
        fun gain(speed: Float): Double = (BASE_GAIN + ACCEL * speed).coerceIn(BASE_GAIN, MAX_GAIN)
    }
}

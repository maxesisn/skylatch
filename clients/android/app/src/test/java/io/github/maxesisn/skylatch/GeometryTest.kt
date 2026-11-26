// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch

import io.github.maxesisn.skylatch.input.Box
import io.github.maxesisn.skylatch.input.Geometry
import io.github.maxesisn.skylatch.input.Trackpad
import io.github.maxesisn.skylatch.input.Zoom
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.math.hypot

class GeometryTest {
    @Test
    fun letterboxKeepsTheVideoAspectCentred() {
        assertEquals(Box(164f, 0f, 1920f, 1080f), Geometry.letterbox(2248f, 1080f, 3840, 2160))
        assertEquals(Box(260f, 0f, 1728f, 1080f), Geometry.letterbox(2248f, 1080f, 2560, 1600))
        assertEquals(Box(0f, 202f, 1080f, 675f), Geometry.letterbox(1080f, 1080f, 1920, 1200))
        assertEquals(Box.EMPTY, Geometry.letterbox(1080f, 1920f, 0, 0))
    }

    @Test
    fun directTouchNormalizesInsideTheVideoRect() {
        val r = Geometry.letterbox(2248f, 1080f, 3840, 2160)
        assertEquals(0, Geometry.normalize(r.left, r.left, r.width))
        assertEquals(0, Geometry.normalize(10f, r.left, r.width))
        assertEquals(65535, Geometry.normalize(r.right - 1, r.left, r.width))
        assertEquals(65535, Geometry.normalize(5000f, r.left, r.width))
        assertEquals(32767, Geometry.normalize(r.left + (r.width - 1) / 2, r.left, r.width))
        assertEquals(r.left, Geometry.denormalize(0, r.left, r.width), 0.001f)
        assertEquals(r.right - 1, Geometry.denormalize(65535, r.left, r.width), 0.001f)
        val zoomed = Geometry.zoomed(r, Zoom(2f, -960f, -540f))
        assertEquals(r.left + (r.width - 1) / 2, zoomed.left + (zoomed.width - 1) / 2, 0.5f)
        assertEquals(32767, Geometry.normalize(zoomed.left + (zoomed.width - 1) / 2, zoomed.left, zoomed.width))
    }

    @Test
    fun pinchZoomAnchorsTheFocusAndStaysInsideTheFittedRect() {
        val base = Box(100f, 0f, 1000f, 500f)
        assertEquals(Zoom(1f, 0f, 0f), Geometry.clampZoom(base, Zoom(0.5f, 40f, -40f)))
        assertEquals(Zoom(4f, -3000f, 0f), Geometry.clampZoom(base, Zoom(9f, -9999f, 10f)))
        val z = Geometry.pinch(base, Zoom(), 600f, 250f, 600f, 250f, 2f)
        assertEquals(Zoom(2f, -500f, -250f), z)
        assertEquals(Box(-400f, -250f, 2000f, 1000f), Geometry.zoomed(base, z))
        assertEquals(-600f, Geometry.follow(base, z, 1150f, 250f, 50f).tx, 0.01f)
        assertEquals(z, Geometry.follow(base, z, 600f, 250f, 50f))
        assertEquals(Zoom(), Geometry.follow(base, Zoom(), 5000f, 250f, 50f))
    }

    @Test
    fun trackpadMovesRelativelyWithAcceleration() {
        val view = Box(0f, 0f, 1000f, 500f)
        val pad = Trackpad()
        pad.set(0, 0)
        assertTrue(pad.move(100f, 50f, 100f, view))
        val g = Trackpad.gain(hypot(100f, 50f) / 100f)
        assertEquals((100 * g * 65535 / 1000).toInt(), pad.wireX)
        assertEquals((50 * g * 65535 / 500).toInt(), pad.wireY)
        assertTrue(Trackpad.gain(2f) > Trackpad.gain(0.2f))
        assertEquals(Trackpad.MAX_GAIN, Trackpad.gain(100f), 0.0)
        assertEquals(Trackpad.BASE_GAIN, Trackpad.gain(0f), 0.0)
        pad.move(-100000f, 100000f, 1f, view)
        assertEquals(0, pad.wireX)
        assertEquals(65535, pad.wireY)
        assertFalse(pad.move(0f, 0f, 16f, view))
        assertFalse(pad.move(5f, 5f, 16f, Box.EMPTY))
    }
}

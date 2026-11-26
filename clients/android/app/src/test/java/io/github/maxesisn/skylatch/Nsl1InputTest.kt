// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch

import io.github.maxesisn.skylatch.input.Nsl1Input
import io.github.maxesisn.skylatch.input.ScrollEncoder
import io.github.maxesisn.skylatch.input.WheelStep
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class Nsl1InputTest {
    private fun bytes(vararg v: Int) = ByteArray(v.size) { v[it].toByte() }

    @Test
    fun keyRecordsCarryHidUsageAndMasks() {
        assertArrayEquals(bytes(1, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1), Nsl1Input.keyDown(4, modifiers = 1))
        assertArrayEquals(bytes(2, 0, 0, 224, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0), Nsl1Input.keyUp(224))
    }

    @Test
    fun pointerAndButtonRecordsAreBigEndian() {
        assertArrayEquals(
            bytes(3, 0, 0, 0, 0, 0, 0, 0, 0x12, 0x34, 0xff, 0xff, 0, 0, 0, 0),
            Nsl1Input.pointerMove(0x1234, 65535),
        )
        assertArrayEquals(
            bytes(4, 0, 0, 3, 0, 0, 0, 0, 0, 10, 0, 20, 0, 4, 0, 0),
            Nsl1Input.buttonDown(Nsl1Input.BUTTON_RIGHT, 10, 20, buttons = Nsl1Input.buttonBit(3)),
        )
        assertArrayEquals(
            bytes(5, 0, 0, 1, 0, 0, 0, 0, 0, 10, 0, 20, 0, 0, 0, 0),
            Nsl1Input.buttonUp(Nsl1Input.BUTTON_LEFT, 10, 20, buttons = 0),
        )
        assertEquals(0, Nsl1Input.pointerMove(-5, 70000)[8].toInt())
        assertEquals(0xff.toByte(), Nsl1Input.pointerMove(-5, 70000)[10])
    }

    @Test
    fun wheelAndSmoothWheelValuesAreSigned() {
        assertArrayEquals(
            bytes(6, 0, 0, 0, 0xff, 0xff, 0xff, 0x10, 0, 1, 0, 2, 0, 0, 0, 0),
            Nsl1Input.wheel(Nsl1Input.AXIS_VERTICAL, -240, 1, 2),
        )
        assertArrayEquals(
            bytes(9, 0, 0, 1, 0, 0, 0, 37, 0, 1, 0, 2, 0, 0, 0, 0),
            Nsl1Input.smoothWheel(Nsl1Input.AXIS_HORIZONTAL, 37, 1, 2),
        )
        assertThrows { Nsl1Input.wheel(0, 100, 0, 0) }
        assertThrows { Nsl1Input.wheel(0, 1320, 0, 0) }
        assertThrows { Nsl1Input.smoothWheel(0, 0, 0, 0) }
        assertThrows { Nsl1Input.smoothWheel(0, Nsl1Input.MAX_SMOOTH + 1, 0, 0) }
    }

    @Test
    fun textRecordsAreUnicodeScalars() {
        assertArrayEquals(bytes(8, 0, 0, 0, 0, 0, 0x6f, 0x22, 0, 0, 0, 0, 0, 0, 0, 0), Nsl1Input.text('漢'.code))
        val records = Nsl1Input.textRecords("中a\uD83D\uDE00\n")
        assertEquals(3, records.size)
        assertEquals(0x1F600, (records[2][5].toInt() and 0xff shl 16) or (records[2][6].toInt() and 0xff shl 8) or (records[2][7].toInt() and 0xff))
        assertFalse(Nsl1Input.isTextScalar(0xD800))
        assertFalse(Nsl1Input.isTextScalar(0))
        assertTrue(Nsl1Input.isTextScalar(0x10FFFF))
        assertArrayEquals(bytes(7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0), Nsl1Input.releaseAll())
    }

    @Test
    fun modifierBitsFollowHidOrder() {
        assertEquals(1, Nsl1Input.modifierBit(224))
        assertEquals(8, Nsl1Input.modifierBit(227))
        assertEquals(0x80, Nsl1Input.modifierBit(231))
        assertEquals(0, Nsl1Input.modifierBit(4))
    }

    @Test
    fun scrollFoldsIntoNotchesUntilTheHostAcceptsSmoothWheel() {
        val enc = ScrollEncoder(pixelsPerNotch = 50.0)
        assertFalse(enc.smooth)
        assertEquals(emptyList<WheelStep>(), enc.pixels(0.0, 30.0))
        assertEquals(listOf(WheelStep(Nsl1Input.WHEEL, 0, 120)), enc.pixels(0.0, 30.0))
        assertEquals(listOf(WheelStep(Nsl1Input.WHEEL, 0, -120)), enc.pixels(0.0, -60.0))
        assertEquals(listOf(WheelStep(Nsl1Input.WHEEL, 0, 1200), WheelStep(Nsl1Input.WHEEL, 0, 240)), enc.pixels(0.0, 600.0))

        enc.onInputResponse(7, Nsl1Input.STATUS_ACCEPTED)
        assertFalse(enc.smooth)
        enc.onInputResponse(0, 2)
        assertFalse(enc.smooth)
        enc.onInputResponse(0, Nsl1Input.STATUS_ACCEPTED)
        assertTrue(enc.smooth)

        assertEquals(listOf(WheelStep(Nsl1Input.SMOOTH_WHEEL, 0, 24), WheelStep(Nsl1Input.SMOOTH_WHEEL, 1, -16)), enc.pixels(-1.0, 1.5))
        assertEquals(emptyList<WheelStep>(), enc.pixels(0.0, 0.04))
        assertEquals(listOf(WheelStep(Nsl1Input.SMOOTH_WHEEL, 0, 1)), enc.pixels(0.0, 0.04))
        val big = enc.pixels(0.0, 20000.0)
        assertEquals(listOf(Nsl1Input.MAX_SMOOTH, 20000 * 16 - Nsl1Input.MAX_SMOOTH), big.map { it.value })
        assertEquals(Nsl1Input.SMOOTH_WHEEL, enc.record(big[0], 1, 2, 0, 0)[0].toInt())
    }

    @Test
    fun mouseDetentsAlwaysUseNotches() {
        val enc = ScrollEncoder()
        enc.onInputResponse(0, Nsl1Input.STATUS_ACCEPTED)
        assertEquals(listOf(WheelStep(Nsl1Input.WHEEL, 0, -120), WheelStep(Nsl1Input.WHEEL, 1, 240)), enc.notches(2.0, -1.0))
        assertEquals(emptyList<WheelStep>(), enc.notches(0.0, 0.5))
        assertEquals(listOf(WheelStep(Nsl1Input.WHEEL, 0, 120)), enc.notches(0.0, 0.5))
        assertEquals(Nsl1Input.WHEEL, enc.record(WheelStep(Nsl1Input.WHEEL, 0, 120), 0, 0, 0, 0)[0].toInt())
    }

    private fun assertThrows(block: () -> Unit) {
        try {
            block()
        } catch (_: IllegalArgumentException) {
            return
        }
        throw AssertionError("expected IllegalArgumentException")
    }
}

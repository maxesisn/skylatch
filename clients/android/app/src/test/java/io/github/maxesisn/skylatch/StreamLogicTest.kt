// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch

import io.github.maxesisn.skylatch.input.KeyMap
import io.github.maxesisn.skylatch.stream.AnnexB
import io.github.maxesisn.skylatch.stream.PcmRing
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class StreamLogicTest {
    private fun bytes(vararg v: Int) = ByteArray(v.size) { v[it].toByte() }

    @Test
    fun annexBFindsParameterSetsBehindEitherStartCode() {
        val au = bytes(0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1f, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80, 0, 0, 0, 1, 0x65, 0x88, 0x84)
        assertEquals(listOf(7, 8, 5), AnnexB.nals(au).map { it.type })
        val sets = AnnexB.parameterSets(au)!!
        assertArrayEquals(bytes(0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1f), sets.sps)
        assertArrayEquals(bytes(0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80), sets.pps)
        assertEquals(sets, AnnexB.parameterSets(au.copyOf()))

        val p = bytes(0, 0, 1, 0x41, 0x9a, 0x02)
        assertNull(AnnexB.parameterSets(p))
        assertEquals(emptyList<AnnexB.Nal>(), AnnexB.nals(bytes(1, 2, 3)))
    }

    @Test
    fun jitterBufferPrimesAndDropsTheOldestBacklog() {
        val ring = PcmRing(capacity = 100, startSamples = 10, maxSamples = 30)
        val out = ShortArray(50)
        ring.write(ShortArray(6) { it.toShort() })
        assertEquals(0, ring.read(out))
        ring.write(ShortArray(6) { (6 + it).toShort() })
        assertEquals(12, ring.read(out))
        assertEquals(11, out[11].toInt())
        assertFalse(ring.primed)

        ring.write(ShortArray(40) { it.toShort() })
        assertEquals(10, ring.size)
        assertEquals(30L, ring.dropped)
        assertEquals(10, ring.read(out, 50))
        assertEquals(30, out[0].toInt())

        ring.write(ShortArray(20))
        ring.clear()
        assertEquals(0, ring.size)
    }

    @Test
    fun keyMapCoversLettersDigitsFunctionKeysAndModifiers() {
        assertEquals(4, KeyMap.hid(android.view.KeyEvent.KEYCODE_A))
        assertEquals(29, KeyMap.hid(android.view.KeyEvent.KEYCODE_Z))
        assertEquals(30, KeyMap.hid(android.view.KeyEvent.KEYCODE_1))
        assertEquals(39, KeyMap.hid(android.view.KeyEvent.KEYCODE_0))
        assertEquals(58, KeyMap.hid(android.view.KeyEvent.KEYCODE_F1))
        assertEquals(69, KeyMap.hid(android.view.KeyEvent.KEYCODE_F12))
        assertEquals(KeyMap.UP, KeyMap.hid(android.view.KeyEvent.KEYCODE_DPAD_UP))
        assertEquals(KeyMap.BACKSPACE, KeyMap.hid(android.view.KeyEvent.KEYCODE_DEL))
        assertEquals(KeyMap.DELETE, KeyMap.hid(android.view.KeyEvent.KEYCODE_FORWARD_DEL))
        assertEquals(224, KeyMap.hid(android.view.KeyEvent.KEYCODE_CTRL_LEFT))
        assertEquals(231, KeyMap.hid(android.view.KeyEvent.KEYCODE_META_RIGHT))
        assertTrue(KeyMap.isModifier(226))
        assertNull(KeyMap.hid(android.view.KeyEvent.KEYCODE_BACK))
        assertEquals(6, KeyMap.hidForChar('C'))
        assertEquals(25, KeyMap.hidForChar('v'))
        assertNull(KeyMap.hidForChar('中'))
    }
}

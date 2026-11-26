// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch

import io.github.maxesisn.skylatch.stream.HudMode
import io.github.maxesisn.skylatch.stream.HudPlacement
import io.github.maxesisn.skylatch.stream.PerfAudio
import io.github.maxesisn.skylatch.stream.PerfFormat
import io.github.maxesisn.skylatch.stream.PerfPath
import io.github.maxesisn.skylatch.stream.PerfRates
import io.github.maxesisn.skylatch.stream.PerfReadout
import io.github.maxesisn.skylatch.stream.PerfRow
import io.github.maxesisn.skylatch.stream.PerfStats
import io.github.maxesisn.skylatch.stream.PerfWindow
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PerfStatsTest {
    private val ms = 1_000_000L

    @Test
    fun windowAveragesTheLastSecond() {
        val w = PerfWindow()
        for (i in 0 until 60) {
            val at = 1000 * ms + i * 16 * ms
            w.received(12_500, at)
            w.decoded(4_000, at)
            w.presented(2_000, at)
        }
        w.audioBuffered(40.0, 1900 * ms)
        val r = w.rates(1960 * ms)
        assertEquals(60.0, r.fps, 0.0)
        assertEquals(6.0, r.mbps, 1e-9)
        assertEquals(4.0, r.decodeMs!!, 1e-9)
        assertEquals(2.0, r.waitMs!!, 1e-9)
        assertEquals(40.0, r.audioMs!!, 1e-9)

        val later = w.rates(2500 * ms)
        assertEquals(28.0, later.fps, 0.0)
        val empty = w.rates(5000 * ms)
        assertEquals(PerfRates(), empty)
        assertNull(empty.decodeMs)
    }

    @Test
    fun statsUseTheInjectedClock() {
        var now = 10_000 * ms
        val s = PerfStats { now }
        s.decoder = "HW"
        s.width = 3840
        s.height = 2160
        repeat(30) { s.presented(1_000) }
        s.received(1_000_000)
        val r = s.readout(PerfPath.DIRECT, PerfAudio.ON)
        assertEquals(30.0, r.rates.fps, 0.0)
        assertEquals(8.0, r.rates.mbps, 1e-9)
        assertEquals("HW", r.decoder)
        now += 1001 * ms
        assertEquals(0.0, s.readout(PerfPath.DIRECT, PerfAudio.ON).rates.fps, 0.0)
    }

    @Test
    fun hudPositionIsClampedInsideTheBounds() {
        assertEquals(0f to 0f, HudPlacement.clamp(-20f, -5f, 200f, 60f, 1000f, 500f))
        assertEquals(800f to 440f, HudPlacement.clamp(900f, 900f, 200f, 60f, 1000f, 500f))
        assertEquals(120f to 80f, HudPlacement.clamp(120f, 80f, 200f, 60f, 1000f, 500f))
        assertEquals(0f to 0f, HudPlacement.clamp(50f, 50f, 1200f, 600f, 1000f, 500f))
    }

    @Test
    fun hudCyclesHiddenCardStrip() {
        assertEquals(HudMode.CARD, HudMode.HIDDEN.next)
        assertEquals(HudMode.STRIP, HudMode.CARD.next)
        assertEquals(HudMode.HIDDEN, HudMode.STRIP.next)
    }

    @Test
    fun hudRowsFormatTheReadout() {
        val r = PerfReadout(3840, 2160, "HW", PerfPath.RELAY, PerfAudio.ON, PerfRates(59.6, 18.24, 3.21, 1.04, 41.6))
        assertEquals(
            listOf(
                PerfRow("60 FPS\u20033840\u00D72160", "中继"),
                PerfRow("解 HW 3.2ms", "等待 1.0ms"),
                PerfRow("码率 18.2 Mbps", "音频 Opus 42ms"),
            ),
            PerfFormat.card(r),
        )
        assertEquals("60 FPS\u20033840\u00D72160\u2003HW 3.2ms\u200318.2 Mbps\u2003中继", PerfFormat.strip(r))
        val idle = PerfReadout(audio = PerfAudio.MUTED)
        assertEquals(PerfRow("解 --", "等待 --"), PerfFormat.card(idle)[1])
        assertEquals("音频 已静音", PerfFormat.card(idle)[2].right)
        assertTrue(idle.isSlow)
    }
}

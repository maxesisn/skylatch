// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.input

import kotlin.math.abs
import kotlin.math.max
import kotlin.math.min
import kotlin.math.truncate

/**
 * 16-byte NSL1 input records (`docs/PROTOCOL.md`, `crates/skylatch-proto/src/nsl1.rs`): type u8,
 * reserved u8, code u16, value i32, x u16, y u16, button mask u16, modifier mask u16, big-endian.
 */
object Nsl1Input {
    const val SIZE = 16

    const val KEY_DOWN = 1
    const val KEY_UP = 2
    const val POINTER_MOVE = 3
    const val BUTTON_DOWN = 4
    const val BUTTON_UP = 5
    const val WHEEL = 6
    const val RELEASE_ALL = 7
    const val TEXT = 8
    const val SMOOTH_WHEEL = 9

    const val BUTTON_LEFT = 1
    const val BUTTON_MIDDLE = 2
    const val BUTTON_RIGHT = 3

    const val AXIS_VERTICAL = 0
    const val AXIS_HORIZONTAL = 1

    const val NOTCH = 120
    const val MAX_WHEEL = 1200
    const val MAX_SMOOTH = 262_144
    const val MAX_COORD = 65_535

    /** Input-response status that accepts the smooth-wheel query. */
    const val STATUS_ACCEPTED = 0

    fun record(
        type: Int,
        code: Int = 0,
        value: Int = 0,
        x: Int = 0,
        y: Int = 0,
        buttons: Int = 0,
        modifiers: Int = 0,
    ): ByteArray {
        val out = ByteArray(SIZE)
        out[0] = type.toByte()
        putU16(out, 2, code)
        out[4] = (value ushr 24).toByte()
        out[5] = (value ushr 16).toByte()
        out[6] = (value ushr 8).toByte()
        out[7] = value.toByte()
        putU16(out, 8, x.coerceIn(0, MAX_COORD))
        putU16(out, 10, y.coerceIn(0, MAX_COORD))
        putU16(out, 12, buttons and 0x1f)
        putU16(out, 14, modifiers and 0xff)
        return out
    }

    private fun putU16(out: ByteArray, at: Int, v: Int) {
        out[at] = (v ushr 8).toByte()
        out[at + 1] = v.toByte()
    }

    fun keyDown(usage: Int, buttons: Int = 0, modifiers: Int = 0): ByteArray {
        require(usage in 0..231) { "HID usage $usage" }
        return record(KEY_DOWN, code = usage, buttons = buttons, modifiers = modifiers)
    }

    fun keyUp(usage: Int, buttons: Int = 0, modifiers: Int = 0): ByteArray {
        require(usage in 0..231) { "HID usage $usage" }
        return record(KEY_UP, code = usage, buttons = buttons, modifiers = modifiers)
    }

    fun pointerMove(x: Int, y: Int, buttons: Int = 0, modifiers: Int = 0): ByteArray =
        record(POINTER_MOVE, x = x, y = y, buttons = buttons, modifiers = modifiers)

    fun buttonDown(button: Int, x: Int, y: Int, buttons: Int, modifiers: Int = 0): ByteArray {
        require(button in 1..3) { "button $button" }
        return record(BUTTON_DOWN, code = button, x = x, y = y, buttons = buttons, modifiers = modifiers)
    }

    fun buttonUp(button: Int, x: Int, y: Int, buttons: Int, modifiers: Int = 0): ByteArray {
        require(button in 1..3) { "button $button" }
        return record(BUTTON_UP, code = button, x = x, y = y, buttons = buttons, modifiers = modifiers)
    }

    fun wheel(axis: Int, delta: Int, x: Int, y: Int, buttons: Int = 0, modifiers: Int = 0): ByteArray {
        require(delta % NOTCH == 0 && abs(delta) <= MAX_WHEEL) { "wheel delta $delta" }
        return record(WHEEL, code = axis, value = delta, x = x, y = y, buttons = buttons, modifiers = modifiers)
    }

    fun smoothWheel(axis: Int, sixteenths: Int, x: Int, y: Int, buttons: Int = 0, modifiers: Int = 0): ByteArray {
        require(sixteenths != 0 && abs(sixteenths) <= MAX_SMOOTH) { "smooth wheel delta $sixteenths" }
        return record(SMOOTH_WHEEL, code = axis, value = sixteenths, x = x, y = y, buttons = buttons, modifiers = modifiers)
    }

    fun releaseAll(): ByteArray = record(RELEASE_ALL)

    fun isTextScalar(scalar: Int): Boolean = scalar in 1..0x10FFFF && scalar !in 0xD800..0xDFFF

    fun text(scalar: Int): ByteArray {
        require(isTextScalar(scalar)) { "scalar $scalar" }
        return record(TEXT, value = scalar)
    }

    /** One type 8 record per Unicode scalar; control characters and lone surrogates are skipped. */
    fun textRecords(s: String): List<ByteArray> {
        val out = ArrayList<ByteArray>()
        var i = 0
        while (i < s.length) {
            val cp = s.codePointAt(i)
            i += Character.charCount(cp)
            if (cp >= 0x20 && cp != 0x7F && isTextScalar(cp)) out += text(cp)
        }
        return out
    }

    fun buttonBit(button: Int): Int = 1 shl (button - 1)

    /** Modifier-mask bit of a HID modifier usage (224 left Ctrl through 231 right GUI), else 0. */
    fun modifierBit(usage: Int): Int = if (usage in 224..231) 1 shl (usage - 224) else 0

    /** Splits whole notches into type 6 deltas of at most ten notches each. */
    fun notchChunks(notches: Int): List<Int> {
        val out = ArrayList<Int>()
        var left = notches
        while (left != 0) {
            val part = max(-10, min(10, left))
            out += part * NOTCH
            left -= part
        }
        return out
    }

    /** Splits 1/16 pixel deltas into type 9 values within the wire range. */
    fun smoothChunks(sixteenths: Long): List<Int> {
        val out = ArrayList<Int>()
        var left = sixteenths
        while (left != 0L) {
            val part = max(-MAX_SMOOTH.toLong(), min(MAX_SMOOTH.toLong(), left))
            out += part.toInt()
            left -= part
        }
        return out
    }
}

/** One axis worth of wheel input, before it becomes a record. */
data class WheelStep(val type: Int, val axis: Int, val value: Int)

/**
 * Turns scroll distances into wheel records. Pixel scrolling goes out as type 9 once the host
 * accepted the smooth-wheel query; until then (and for hosts that decline) it folds into type 6
 * notches. Distances are in wire direction: positive scrolls down or right.
 */
class ScrollEncoder(private val pixelsPerNotch: Double = 48.0) {
    @Volatile
    var smooth = false
        private set

    private val notchRemainder = DoubleArray(2)
    private val pixelRemainder = DoubleArray(2)

    /** Applies an input response; sequence 0 answers the query the session sends. */
    fun onInputResponse(sequence: Int, status: Int) {
        if (sequence == 0) smooth = status == Nsl1Input.STATUS_ACCEPTED
    }

    fun reset() {
        notchRemainder.fill(0.0)
        pixelRemainder.fill(0.0)
    }

    /** Pixel distances from a touch gesture or a precision touchpad. Vertical comes first. */
    fun pixels(dx: Double, dy: Double): List<WheelStep> {
        val out = ArrayList<WheelStep>()
        for ((axis, d) in listOf(Nsl1Input.AXIS_VERTICAL to dy, Nsl1Input.AXIS_HORIZONTAL to dx)) {
            if (d == 0.0 || !d.isFinite()) continue
            if (smooth) {
                val total = pixelRemainder[axis] + d * 16
                val whole = truncate(total)
                pixelRemainder[axis] = total - whole
                Nsl1Input.smoothChunks(whole.toLong()).forEach { out += WheelStep(Nsl1Input.SMOOTH_WHEEL, axis, it) }
            } else {
                takeNotches(axis, d / pixelsPerNotch).forEach { out += WheelStep(Nsl1Input.WHEEL, axis, it) }
            }
        }
        return out
    }

    /** Wheel detents from a mouse; fractions accumulate until they make a whole notch. */
    fun notches(dx: Double, dy: Double): List<WheelStep> {
        val out = ArrayList<WheelStep>()
        for ((axis, d) in listOf(Nsl1Input.AXIS_VERTICAL to dy, Nsl1Input.AXIS_HORIZONTAL to dx)) {
            if (d == 0.0 || !d.isFinite()) continue
            takeNotches(axis, d).forEach { out += WheelStep(Nsl1Input.WHEEL, axis, it) }
        }
        return out
    }

    private fun takeNotches(axis: Int, notches: Double): List<Int> {
        val r = notchRemainder[axis]
        var total = if ((notches < 0 && r > 0) || (notches > 0 && r < 0)) notches else r + notches
        val whole = truncate(total).toInt()
        total -= whole
        notchRemainder[axis] = total
        return Nsl1Input.notchChunks(whole)
    }

    fun record(step: WheelStep, x: Int, y: Int, buttons: Int, modifiers: Int): ByteArray =
        if (step.type == Nsl1Input.SMOOTH_WHEEL) {
            Nsl1Input.smoothWheel(step.axis, step.value, x, y, buttons, modifiers)
        } else {
            Nsl1Input.wheel(step.axis, step.value, x, y, buttons, modifiers)
        }
}

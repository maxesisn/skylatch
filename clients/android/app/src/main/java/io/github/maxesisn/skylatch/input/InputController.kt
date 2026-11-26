// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.input

import android.os.Handler
import android.os.Looper
import android.view.HapticFeedbackConstants
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlin.math.abs
import kotlin.math.hypot

/**
 * Touch, mouse and keyboard input for one stream. Trackpad mode moves the remote pointer
 * relatively; direct mode puts it under the finger. A hardware mouse always maps absolutely.
 */
class InputController(
    private val send: (ByteArray) -> Unit,
    private val scroll: ScrollEncoder,
    /** HID usage of the shortcut modifier on the host: left GUI (Cmd) on macOS, left Ctrl elsewhere. */
    val primaryModifier: Int,
) {
    var trackpad by mutableStateOf(true)
    var zoom by mutableStateOf(Zoom())
    var cursorX by mutableIntStateOf(32767)
        private set
    var cursorY by mutableIntStateOf(32767)
        private set
    var usingMouse by mutableStateOf(false)
        private set

    /** Modifiers latched from the shortcut bar (HID usages), applied to the next key and then cleared. */
    var latched by mutableStateOf(emptySet<Int>())
        private set

    var base: Box = Box.EMPTY
        set(v) {
            field = v
            zoom = Geometry.clampZoom(v, zoom)
        }
    var videoWidth = 0
    var videoHeight = 0

    val display: Box get() = Geometry.zoomed(base, zoom)

    private val main = Handler(Looper.getMainLooper())
    private val pad = Trackpad()
    private val held = HashSet<Int>()
    private var modifiers = 0
    private var buttons = 0
    private var slop = 16f

    private var downX = 0f
    private var downY = 0f
    private var downTime = 0L
    private var lastX = 0f
    private var lastY = 0f
    private var lastTime = 0L
    private var fingers = 0
    private var moved = false
    private var dragging = false
    private var dragArmed = false
    private var longFired = false
    private var lastTapUp = 0L
    private var lastTapX = 0f
    private var lastTapY = 0f

    private enum class Two { NONE, SCROLL, PINCH }

    private var two = Two.NONE
    private var twoStart = 0L
    private var span0 = 1f
    private var cx0 = 0f
    private var cy0 = 0f
    private var lastCx = 0f
    private var lastCy = 0f
    private var zoom0 = Zoom()

    private var view: View? = null

    private val longPress = Runnable {
        if (fingers == 1 && !moved && !trackpad) {
            longFired = true
            view?.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
            click(Nsl1Input.BUTTON_RIGHT)
        }
    }

    fun attach(v: View) {
        view = v
        slop = ViewConfiguration.get(v.context).scaledTouchSlop.toFloat()
    }

    private fun mask() = modifiers or latchedMask()

    private fun latchedMask() = latched.fold(0) { m, u -> m or Nsl1Input.modifierBit(u) }

    private fun setCursor(x: Int, y: Int) {
        cursorX = x
        cursorY = y
    }

    private fun moveTo(x: Int, y: Int) {
        setCursor(x, y)
        send(Nsl1Input.pointerMove(x, y, buttons, mask()))
    }

    private fun absolute(x: Float, y: Float): Pair<Int, Int> {
        val d = display
        return Geometry.normalize(x, d.left, d.width) to Geometry.normalize(y, d.top, d.height)
    }

    private fun click(button: Int) {
        val bit = Nsl1Input.buttonBit(button)
        send(Nsl1Input.buttonDown(button, cursorX, cursorY, buttons or bit, mask()))
        send(Nsl1Input.buttonUp(button, cursorX, cursorY, buttons and bit.inv(), mask()))
    }

    private fun press(button: Int, down: Boolean) {
        val bit = Nsl1Input.buttonBit(button)
        buttons = if (down) buttons or bit else buttons and bit.inv()
        send(
            if (down) Nsl1Input.buttonDown(button, cursorX, cursorY, buttons, mask())
            else Nsl1Input.buttonUp(button, cursorX, cursorY, buttons, mask()),
        )
    }

    private fun followCursor() {
        if (zoom.scale <= 1f) return
        val d = display
        val sx = Geometry.denormalize(cursorX, d.left, d.width)
        val sy = Geometry.denormalize(cursorY, d.top, d.height)
        zoom = Geometry.follow(base, zoom, sx, sy, minOf(base.width, base.height) * 0.08f)
    }

    // Touch

    fun onTouch(ev: MotionEvent): Boolean {
        if (ev.getToolType(0) == MotionEvent.TOOL_TYPE_MOUSE) return onMouseTouch(ev)
        usingMouse = false
        when (ev.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                fingers = 1
                moved = false
                dragging = false
                longFired = false
                two = Two.NONE
                downX = ev.x; downY = ev.y; downTime = ev.eventTime
                lastX = ev.x; lastY = ev.y; lastTime = ev.eventTime
                if (trackpad) {
                    pad.set(cursorX, cursorY)
                    dragArmed = ev.eventTime - lastTapUp < DOUBLE_TAP_MS && hypot(ev.x - lastTapX, ev.y - lastTapY) < slop * 4
                } else {
                    val (x, y) = absolute(ev.x, ev.y)
                    moveTo(x, y)
                    main.postDelayed(longPress, LONG_PRESS_MS)
                }
            }
            MotionEvent.ACTION_POINTER_DOWN -> {
                fingers = maxOf(fingers, ev.pointerCount)
                main.removeCallbacks(longPress)
                if (dragging) {
                    press(Nsl1Input.BUTTON_LEFT, false)
                    dragging = false
                }
                if (ev.pointerCount == 2) startTwo(ev)
            }
            MotionEvent.ACTION_MOVE -> {
                if (fingers >= 2) {
                    if (ev.pointerCount >= 2) moveTwo(ev)
                } else {
                    oneFingerMove(ev)
                }
            }
            MotionEvent.ACTION_UP -> {
                main.removeCallbacks(longPress)
                if (fingers == 1) oneFingerUp(ev)
                else if (fingers == 2 && two == Two.NONE && ev.eventTime - twoStart < TWO_TAP_MS) {
                    if (!trackpad) {
                        val (x, y) = absolute(cx0, cy0)
                        moveTo(x, y)
                    }
                    click(Nsl1Input.BUTTON_RIGHT)
                }
                fingers = 0
            }
            MotionEvent.ACTION_CANCEL -> {
                main.removeCallbacks(longPress)
                if (dragging) press(Nsl1Input.BUTTON_LEFT, false)
                dragging = false
                fingers = 0
            }
        }
        return true
    }

    private fun oneFingerMove(ev: MotionEvent) {
        if (!moved && hypot(ev.x - downX, ev.y - downY) > slop) {
            moved = true
            main.removeCallbacks(longPress)
            if (trackpad) {
                if (dragArmed) {
                    press(Nsl1Input.BUTTON_LEFT, true)
                    dragging = true
                }
            } else if (!longFired) {
                press(Nsl1Input.BUTTON_LEFT, true)
                dragging = true
            }
        }
        if (moved && !longFired) {
            if (trackpad) {
                if (pad.move(ev.x - lastX, ev.y - lastY, (ev.eventTime - lastTime).toFloat(), display)) {
                    moveTo(pad.wireX, pad.wireY)
                    followCursor()
                }
            } else {
                val (x, y) = absolute(ev.x, ev.y)
                moveTo(x, y)
            }
        }
        lastX = ev.x; lastY = ev.y; lastTime = ev.eventTime
    }

    private fun oneFingerUp(ev: MotionEvent) {
        if (dragging) {
            press(Nsl1Input.BUTTON_LEFT, false)
            dragging = false
            return
        }
        if (longFired || moved) return
        if (trackpad && ev.eventTime - downTime > TAP_MS) return
        click(Nsl1Input.BUTTON_LEFT)
        lastTapUp = ev.eventTime
        lastTapX = ev.x
        lastTapY = ev.y
    }

    private fun startTwo(ev: MotionEvent) {
        two = Two.NONE
        twoStart = ev.eventTime
        span0 = maxOf(1f, hypot(ev.getX(0) - ev.getX(1), ev.getY(0) - ev.getY(1)))
        cx0 = (ev.getX(0) + ev.getX(1)) / 2
        cy0 = (ev.getY(0) + ev.getY(1)) / 2
        lastCx = cx0
        lastCy = cy0
        zoom0 = zoom
    }

    private fun moveTwo(ev: MotionEvent) {
        val span = maxOf(1f, hypot(ev.getX(0) - ev.getX(1), ev.getY(0) - ev.getY(1)))
        val cx = (ev.getX(0) + ev.getX(1)) / 2
        val cy = (ev.getY(0) + ev.getY(1)) / 2
        if (two == Two.NONE) {
            val dSpan = abs(span - span0)
            val dC = hypot(cx - cx0, cy - cy0)
            two = when {
                dSpan > slop * 2 && dSpan > dC -> Two.PINCH
                dC > slop -> Two.SCROLL
                else -> Two.NONE
            }
            if (two == Two.SCROLL) {
                lastCx = cx
                lastCy = cy
                if (!trackpad) {
                    val (x, y) = absolute(cx, cy)
                    moveTo(x, y)
                }
            }
        }
        when (two) {
            Two.PINCH -> zoom = Geometry.pinch(base, zoom0, cx0, cy0, cx, cy, span / span0)
            Two.SCROLL -> {
                val d = display
                val k = if (videoWidth > 0 && d.width > 0) videoWidth / d.width else 1f
                for (step in scroll.pixels((-(cx - lastCx) * k).toDouble(), (-(cy - lastCy) * k).toDouble())) {
                    send(scroll.record(step, cursorX, cursorY, buttons, mask()))
                }
                lastCx = cx
                lastCy = cy
            }
            Two.NONE -> Unit
        }
    }

    // Mouse

    private fun onMouseTouch(ev: MotionEvent): Boolean {
        usingMouse = true
        if (ev.actionMasked == MotionEvent.ACTION_MOVE || ev.actionMasked == MotionEvent.ACTION_DOWN) {
            val (x, y) = absolute(ev.x, ev.y)
            moveTo(x, y)
        }
        return true
    }

    fun onGenericMotion(ev: MotionEvent): Boolean {
        if (!ev.isFromSource(android.view.InputDevice.SOURCE_MOUSE) && ev.getToolType(0) != MotionEvent.TOOL_TYPE_MOUSE) return false
        usingMouse = true
        when (ev.actionMasked) {
            MotionEvent.ACTION_HOVER_ENTER, MotionEvent.ACTION_HOVER_MOVE -> {
                val (x, y) = absolute(ev.x, ev.y)
                moveTo(x, y)
            }
            MotionEvent.ACTION_BUTTON_PRESS, MotionEvent.ACTION_BUTTON_RELEASE -> {
                val button = when (ev.actionButton) {
                    MotionEvent.BUTTON_PRIMARY -> Nsl1Input.BUTTON_LEFT
                    MotionEvent.BUTTON_SECONDARY -> Nsl1Input.BUTTON_RIGHT
                    MotionEvent.BUTTON_TERTIARY -> Nsl1Input.BUTTON_MIDDLE
                    else -> return true
                }
                val (x, y) = absolute(ev.x, ev.y)
                setCursor(x, y)
                press(button, ev.actionMasked == MotionEvent.ACTION_BUTTON_PRESS)
            }
            MotionEvent.ACTION_SCROLL -> {
                val (x, y) = absolute(ev.x, ev.y)
                setCursor(x, y)
                val v = ev.getAxisValue(MotionEvent.AXIS_VSCROLL).toDouble()
                val h = ev.getAxisValue(MotionEvent.AXIS_HSCROLL).toDouble()
                val detents = v == Math.rint(v) && h == Math.rint(h)
                val steps = if (detents || !scroll.smooth) scroll.notches(h, -v) else scroll.pixels(h * SCROLL_PX, -v * SCROLL_PX)
                for (step in steps) send(scroll.record(step, x, y, buttons, mask()))
            }
        }
        return true
    }

    // Keyboard

    /** A key from a hardware keyboard or the IME. False for keys the system should keep. */
    fun onKey(ev: KeyEvent): Boolean {
        when (ev.keyCode) {
            KeyEvent.KEYCODE_BACK, KeyEvent.KEYCODE_VOLUME_UP, KeyEvent.KEYCODE_VOLUME_DOWN,
            KeyEvent.KEYCODE_VOLUME_MUTE, KeyEvent.KEYCODE_POWER, KeyEvent.KEYCODE_HOME,
            KeyEvent.KEYCODE_APP_SWITCH,
            -> return false
        }
        val soft = ev.deviceId == KeyCharacterMap.VIRTUAL_KEYBOARD || ev.flags and KeyEvent.FLAG_SOFT_KEYBOARD != 0
        val usage = KeyMap.hid(ev.keyCode)
        if (usage != null) {
            if (soft) {
                if (ev.action == KeyEvent.ACTION_DOWN) tap(usage)
                return true
            }
            when (ev.action) {
                KeyEvent.ACTION_DOWN -> if (ev.repeatCount == 0) {
                    modifiers = modifiers or Nsl1Input.modifierBit(usage)
                    held += usage
                    send(Nsl1Input.keyDown(usage, buttons, mask()))
                }
                KeyEvent.ACTION_UP -> if (held.remove(usage)) {
                    modifiers = modifiers and Nsl1Input.modifierBit(usage).inv()
                    send(Nsl1Input.keyUp(usage, buttons, mask()))
                }
            }
            return true
        }
        if (ev.action == KeyEvent.ACTION_DOWN) {
            val c = ev.unicodeChar
            if (c != 0 && Nsl1Input.isTextScalar(c)) send(Nsl1Input.text(c))
            return c != 0
        }
        return false
    }

    /** Committed IME text: characters as type 8, newline and tab as keys, ASCII as shortcuts while modifiers are latched. */
    fun commitText(text: String) {
        var i = 0
        while (i < text.length) {
            val cp = text.codePointAt(i)
            i += Character.charCount(cp)
            when {
                cp == '\n'.code -> tap(KeyMap.ENTER)
                cp == '\t'.code -> tap(KeyMap.TAB)
                latched.isNotEmpty() && cp < 0x80 && KeyMap.hidForChar(cp.toChar()) != null -> tap(KeyMap.hidForChar(cp.toChar())!!)
                cp >= 0x20 && Nsl1Input.isTextScalar(cp) -> send(Nsl1Input.text(cp))
            }
        }
    }

    /** Presses and releases one key with the latched modifiers, then clears the latch. */
    fun tap(usage: Int, extra: Set<Int> = emptySet()) {
        val mods = latched + extra
        var m = modifiers
        for (u in mods) {
            m = m or Nsl1Input.modifierBit(u)
            send(Nsl1Input.keyDown(u, buttons, m))
        }
        send(Nsl1Input.keyDown(usage, buttons, m))
        send(Nsl1Input.keyUp(usage, buttons, m))
        for (u in mods.reversed()) {
            m = m and Nsl1Input.modifierBit(u).inv()
            send(Nsl1Input.keyUp(u, buttons, m))
        }
        latched = emptySet()
    }

    fun toggleLatch(usage: Int) {
        latched = if (usage in latched) latched - usage else latched + usage
    }

    fun shortcut(char: Char) {
        KeyMap.hidForChar(char)?.let { tap(it, setOf(primaryModifier)) }
    }

    /** Releases everything held on the host, for focus loss and mode changes. */
    fun releaseAll() {
        main.removeCallbacks(longPress)
        if (held.isNotEmpty() || buttons != 0 || modifiers != 0) send(Nsl1Input.releaseAll())
        held.clear()
        buttons = 0
        modifiers = 0
        dragging = false
        fingers = 0
    }

    companion object {
        const val TAP_MS = 250L
        const val TWO_TAP_MS = 300L
        const val DOUBLE_TAP_MS = 300L
        const val LONG_PRESS_MS = 500L
        const val SCROLL_PX = 48.0
    }
}

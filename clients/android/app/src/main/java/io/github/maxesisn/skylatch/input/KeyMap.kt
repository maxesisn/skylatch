// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.input

import android.view.KeyEvent

/** Android key codes to USB HID usages (keyboard page 0x07). Keys are physical; the host layout decides the character. */
object KeyMap {
    const val ENTER = 40
    const val ESCAPE = 41
    const val BACKSPACE = 42
    const val TAB = 43
    const val SPACE = 44
    const val RIGHT = 79
    const val LEFT = 80
    const val DOWN = 81
    const val UP = 82
    const val DELETE = 76

    const val LEFT_CTRL = 224
    const val LEFT_SHIFT = 225
    const val LEFT_ALT = 226
    const val LEFT_GUI = 227

    private val table: Map<Int, Int> = buildMap {
        for (i in 0 until 26) put(KeyEvent.KEYCODE_A + i, 4 + i)
        for (i in 1..9) put(KeyEvent.KEYCODE_1 + i - 1, 29 + i)
        put(KeyEvent.KEYCODE_0, 39)
        put(KeyEvent.KEYCODE_ENTER, ENTER)
        put(KeyEvent.KEYCODE_ESCAPE, ESCAPE)
        put(KeyEvent.KEYCODE_DEL, BACKSPACE)
        put(KeyEvent.KEYCODE_TAB, TAB)
        put(KeyEvent.KEYCODE_SPACE, SPACE)
        put(KeyEvent.KEYCODE_MINUS, 45)
        put(KeyEvent.KEYCODE_EQUALS, 46)
        put(KeyEvent.KEYCODE_LEFT_BRACKET, 47)
        put(KeyEvent.KEYCODE_RIGHT_BRACKET, 48)
        put(KeyEvent.KEYCODE_BACKSLASH, 49)
        put(KeyEvent.KEYCODE_SEMICOLON, 51)
        put(KeyEvent.KEYCODE_APOSTROPHE, 52)
        put(KeyEvent.KEYCODE_GRAVE, 53)
        put(KeyEvent.KEYCODE_COMMA, 54)
        put(KeyEvent.KEYCODE_PERIOD, 55)
        put(KeyEvent.KEYCODE_SLASH, 56)
        put(KeyEvent.KEYCODE_CAPS_LOCK, 57)
        for (i in 0 until 12) put(KeyEvent.KEYCODE_F1 + i, 58 + i)
        put(KeyEvent.KEYCODE_SYSRQ, 70)
        put(KeyEvent.KEYCODE_SCROLL_LOCK, 71)
        put(KeyEvent.KEYCODE_BREAK, 72)
        put(KeyEvent.KEYCODE_INSERT, 73)
        put(KeyEvent.KEYCODE_MOVE_HOME, 74)
        put(KeyEvent.KEYCODE_PAGE_UP, 75)
        put(KeyEvent.KEYCODE_FORWARD_DEL, DELETE)
        put(KeyEvent.KEYCODE_MOVE_END, 77)
        put(KeyEvent.KEYCODE_PAGE_DOWN, 78)
        put(KeyEvent.KEYCODE_DPAD_RIGHT, RIGHT)
        put(KeyEvent.KEYCODE_DPAD_LEFT, LEFT)
        put(KeyEvent.KEYCODE_DPAD_DOWN, DOWN)
        put(KeyEvent.KEYCODE_DPAD_UP, UP)
        put(KeyEvent.KEYCODE_NUM_LOCK, 83)
        put(KeyEvent.KEYCODE_NUMPAD_DIVIDE, 84)
        put(KeyEvent.KEYCODE_NUMPAD_MULTIPLY, 85)
        put(KeyEvent.KEYCODE_NUMPAD_SUBTRACT, 86)
        put(KeyEvent.KEYCODE_NUMPAD_ADD, 87)
        put(KeyEvent.KEYCODE_NUMPAD_ENTER, 88)
        for (i in 1..9) put(KeyEvent.KEYCODE_NUMPAD_1 + i - 1, 88 + i)
        put(KeyEvent.KEYCODE_NUMPAD_0, 98)
        put(KeyEvent.KEYCODE_NUMPAD_DOT, 99)
        put(KeyEvent.KEYCODE_NUMPAD_EQUALS, 103)
        put(KeyEvent.KEYCODE_MENU, 101)
        put(KeyEvent.KEYCODE_CTRL_LEFT, LEFT_CTRL)
        put(KeyEvent.KEYCODE_SHIFT_LEFT, LEFT_SHIFT)
        put(KeyEvent.KEYCODE_ALT_LEFT, LEFT_ALT)
        put(KeyEvent.KEYCODE_META_LEFT, LEFT_GUI)
        put(KeyEvent.KEYCODE_CTRL_RIGHT, 228)
        put(KeyEvent.KEYCODE_SHIFT_RIGHT, 229)
        put(KeyEvent.KEYCODE_ALT_RIGHT, 230)
        put(KeyEvent.KEYCODE_META_RIGHT, 231)
    }

    fun hid(keyCode: Int): Int? = table[keyCode]

    fun isModifier(usage: Int): Boolean = usage in 224..231

    /** The physical key that types an ASCII letter, digit or space, for shortcuts with latched modifiers. */
    fun hidForChar(c: Char): Int? = when (c.lowercaseChar()) {
        in 'a'..'z' -> 4 + (c.lowercaseChar() - 'a')
        in '1'..'9' -> 30 + (c - '1')
        '0' -> 39
        ' ' -> SPACE
        else -> null
    }
}

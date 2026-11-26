// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.input

import android.annotation.SuppressLint
import android.content.Context
import android.text.InputType
import android.view.MotionEvent
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager

/** Transparent full-screen view that owns touch, mouse and IME input while streaming. */
@SuppressLint("ViewConstructor")
class StreamInputView(context: Context, private val input: InputController) : View(context) {
    init {
        isFocusable = true
        isFocusableInTouchMode = true
        input.attach(this)
    }

    @SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(event: MotionEvent): Boolean = input.onTouch(event)

    override fun onGenericMotionEvent(event: MotionEvent): Boolean =
        input.onGenericMotion(event) || super.onGenericMotionEvent(event)

    override fun onCheckIsTextEditor(): Boolean = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_ACTION_NONE
        return RemoteInputConnection()
    }

    fun showKeyboard() {
        requestFocus()
        context.getSystemService(InputMethodManager::class.java)?.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
    }

    fun hideKeyboard() {
        context.getSystemService(InputMethodManager::class.java)?.hideSoftInputFromWindow(windowToken, 0)
    }

    /** Sends committed text to the host; composing text stays in the IME until it is committed. */
    private inner class RemoteInputConnection : BaseInputConnection(this, false) {
        override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
            editable?.clear()
            if (!text.isNullOrEmpty()) input.commitText(text.toString())
            return true
        }

        override fun finishComposingText(): Boolean {
            val e = editable
            val start = if (e != null) getComposingSpanStart(e) else -1
            val end = if (e != null) getComposingSpanEnd(e) else -1
            val composing = if (e != null && start in 0..end && end <= e.length) e.substring(start, end) else ""
            super.finishComposingText()
            e?.clear()
            if (composing.isNotEmpty()) input.commitText(composing)
            return true
        }

        override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
            repeat(beforeLength.coerceIn(0, 64)) { input.tap(KeyMap.BACKSPACE) }
            repeat(afterLength.coerceIn(0, 64)) { input.tap(KeyMap.DELETE) }
            return true
        }
    }
}

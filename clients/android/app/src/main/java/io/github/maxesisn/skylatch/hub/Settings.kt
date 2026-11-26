// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.hub

import android.content.Context

/** App preferences; the account token lives in its own private preferences file. */
class Settings(context: Context) {
    private val prefs = context.getSharedPreferences("skylatch", Context.MODE_PRIVATE)
    private val account = context.getSharedPreferences("skylatch_account", Context.MODE_PRIVATE)

    var hubUrl: String
        get() = prefs.getString("hub_url", "") ?: ""
        set(v) = prefs.edit().putString("hub_url", v).apply()

    var token: String
        get() = account.getString("token", "") ?: ""
        set(v) = account.edit().putString("token", v).apply()

    var accountName: String
        get() = account.getString("account", "") ?: ""
        set(v) = account.edit().putString("account", v).apply()

    var preferDirect: Boolean
        get() = prefs.getBoolean("prefer_direct", true)
        set(v) = prefs.edit().putBoolean("prefer_direct", v).apply()

    /** `-1` follows the system, `0` light, `1` dark. */
    var darkMode: Int
        get() = prefs.getInt("dark_mode", -1)
        set(v) = prefs.edit().putInt("dark_mode", v).apply()

    var trackpad: Boolean
        get() = prefs.getBoolean("trackpad", true)
        set(v) = prefs.edit().putBoolean("trackpad", v).apply()

    var audio: Boolean
        get() = prefs.getBoolean("audio", true)
        set(v) = prefs.edit().putBoolean("audio", v).apply()

    var clipboardOnResume: Boolean
        get() = prefs.getBoolean("clipboard_on_resume", true)
        set(v) = prefs.edit().putBoolean("clipboard_on_resume", v).apply()

    var room: String
        get() = prefs.getString("room", "") ?: ""
        set(v) = prefs.edit().putString("room", v).apply()
}

// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch

import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.view.KeyEvent
import android.graphics.Color
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.SideEffect
import androidx.core.view.WindowCompat
import io.github.maxesisn.skylatch.input.InputController
import io.github.maxesisn.skylatch.stream.HudMode
import io.github.maxesisn.skylatch.ui.HomeScreen
import io.github.maxesisn.skylatch.ui.SkylatchTheme
import io.github.maxesisn.skylatch.ui.StreamScreen

class MainActivity : ComponentActivity() {
    private lateinit var model: AppModel
    private var input: InputController? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge(navigationBarStyle = SystemBarStyle.auto(Color.TRANSPARENT, Color.TRANSPARENT))
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) window.isNavigationBarContrastEnforced = false
        model = shared ?: AppModel(applicationContext).also {
            shared = it
            Log.i(TAG, "Skylatch ${BuildConfig.VERSION_NAME}, native core ${NativeBridge.nativeVersion()}")
            it.checkUpdate(quiet = true)
        }
        applyDebugExtras(intent)
        setContent {
            val systemDark = isSystemInDarkTheme()
            val dark = when (model.dark) {
                1 -> true
                0 -> false
                else -> systemDark
            }
            val streaming = model.stream != null
            SideEffect {
                WindowCompat.getInsetsController(window, window.decorView).apply {
                    isAppearanceLightStatusBars = !dark && !streaming
                    isAppearanceLightNavigationBars = !dark && !streaming
                }
            }
            SkylatchTheme(dark) {
                val s = model.stream
                if (s != null) StreamScreen(model, s) { input = it } else HomeScreen(model, dark)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        applyDebugExtras(intent)
    }

    /**
     * Debug builds accept a hub and token at launch so a device on a test bench can sign in
     * without typing: `am start -n io.github.maxesisn.skylatch/.MainActivity --es skylatch.hub
     * wss://... --es skylatch.token ... [--es skylatch.connect <device id>] [--es skylatch.hud card]`.
     */
    private fun applyDebugExtras(intent: Intent?) {
        if (!BuildConfig.DEBUG || intent == null) return
        val hub = intent.getStringExtra("skylatch.hub")
        val token = intent.getStringExtra("skylatch.token")
        if (hub != null && token != null) model.useToken(hub, token)
        intent.getStringExtra("skylatch.dark")?.toIntOrNull()?.let(model::setDarkMode)
        intent.getStringExtra("skylatch.connect")?.let {
            model.pendingConnect = it
            model.refresh()
        }
        intent.getStringExtra("skylatch.hud")?.let { mode -> model.debugHud = HudMode.entries.firstOrNull { it.name.equals(mode, true) } }
        model.debugBar = intent.getBooleanExtra("skylatch.bar", false)
        if (intent.getBooleanExtra("skylatch.drawer", false)) model.drawerOpen = true
        if (intent.getBooleanExtra("skylatch.leave", false)) model.leaveStream()
    }

    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        val target = input
        if (target != null && model.stream != null && target.onKey(event)) return true
        return super.dispatchKeyEvent(event)
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus && model.clipboardOnResume) model.stream?.pushClipboard(false)
        if (!hasFocus) input?.releaseAll()
    }

    override fun onDestroy() {
        if (isFinishing) model.leaveStream()
        super.onDestroy()
    }

    companion object {
        private const val TAG = "Skylatch"
        private var shared: AppModel? = null
    }
}

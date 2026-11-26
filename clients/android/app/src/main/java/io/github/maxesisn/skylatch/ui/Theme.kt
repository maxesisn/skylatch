// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.ui

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp

/** The desktop palettes: warm paper in light mode, deep carbon in dark mode. */
@Immutable
data class Palette(
    val canvas: Color,
    val surface: Color,
    val card: Color,
    val ink: Color,
    val muted: Color,
    val hair: Color,
    val wash: Color,
    val accent: Color,
    val onAccent: Color,
    val green: Color,
    val warn: Color,
    val dark: Boolean,
) {
    companion object {
        val LIGHT = Palette(
            canvas = Color(0xFFF6F4EF),
            surface = Color(0xFFFAF9F6),
            card = Color(0xFFFFFFFF),
            ink = Color(0xFF1C1C1E),
            muted = Color(0xFF6B6A64),
            hair = Color(0xFFE2DED4),
            wash = Color(0xFFEDEAE2),
            accent = Color(0xFF2563EB),
            onAccent = Color(0xFFFFFFFF),
            green = Color(0xFF16A34A),
            warn = Color(0xFFC2410C),
            dark = false,
        )
        val DARK = Palette(
            canvas = Color(0xFF141518),
            surface = Color(0xFF1A1C20),
            card = Color(0xFF22252B),
            ink = Color(0xFFECEEF2),
            muted = Color(0xFF8E929B),
            hair = Color(0xFF30343C),
            wash = Color(0xFF2A2E35),
            accent = Color(0xFF3B82F6),
            onAccent = Color(0xFFFFFFFF),
            green = Color(0xFF22C55E),
            warn = Color(0xFFF97316),
            dark = true,
        )
    }
}

/** Micro radii: cards 10dp, controls 8dp, badges 5dp. */
object Radii {
    val card = RoundedCornerShape(10.dp)
    val control = RoundedCornerShape(8.dp)
    val badge = RoundedCornerShape(5.dp)
}

/** Translucent carbon of the stream page's chrome, the same in both themes. */
object Carbon {
    val glass = Color(0xE61A1C20)
    val hud = Color(0x8F141518)
    val ink = Color(0xFFECEEF2)
    val muted = Color(0xFF9CA0A8)
    val soft = Color(0xFFD4D7DD)
    val line = Color(0x33FFFFFF)
    val accent = Color(0xFF3B82F6)
    val green = Color(0xFF22C55E)
    val amber = Color(0xFFF59E0B)
    val danger = Color(0xFFF87171)
}

val LocalPalette = staticCompositionLocalOf { Palette.LIGHT }

@Composable
fun SkylatchTheme(dark: Boolean, content: @Composable () -> Unit) {
    val p = if (dark) Palette.DARK else Palette.LIGHT
    val scheme = if (dark) {
        darkColorScheme(
            primary = p.accent, onPrimary = p.onAccent, background = p.canvas, onBackground = p.ink,
            surface = p.surface, onSurface = p.ink, surfaceVariant = p.wash, onSurfaceVariant = p.muted,
            outline = p.hair, outlineVariant = p.hair, error = p.warn,
        )
    } else {
        lightColorScheme(
            primary = p.accent, onPrimary = p.onAccent, background = p.canvas, onBackground = p.ink,
            surface = p.surface, onSurface = p.ink, surfaceVariant = p.wash, onSurfaceVariant = p.muted,
            outline = p.hair, outlineVariant = p.hair, error = p.warn,
        )
    }
    CompositionLocalProvider(LocalPalette provides p) {
        MaterialTheme(colorScheme = scheme, content = content)
    }
}

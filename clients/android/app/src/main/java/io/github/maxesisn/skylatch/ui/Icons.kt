// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.unit.dp

/** Line icons on a 24-unit grid, stroked like the desktop header icons. */
object SkyIcons {
    private fun icon(name: String, vararg paths: String): ImageVector {
        val b = ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
        for (d in paths) {
            b.addPath(
                pathData = addPathNodes(d),
                stroke = SolidColor(Color.Black),
                strokeLineWidth = 1.8f,
                strokeLineCap = StrokeCap.Round,
                strokeLineJoin = StrokeJoin.Round,
            )
        }
        return b.build()
    }

    val Refresh = icon("refresh", "M20 12a8 8 0 1 1-2.34-5.66", "M20 4v4.5h-4.5")
    val Sun = icon(
        "sun",
        "M12 8.5a3.5 3.5 0 1 0 0 7a3.5 3.5 0 1 0 0-7z",
        "M12 2.5v2M12 19.5v2M4.6 4.6l1.4 1.4M18 18l1.4 1.4M2.5 12h2M19.5 12h2M4.6 19.4L6 18M18 6l1.4-1.4",
    )
    val Moon = icon("moon", "M20 13.5A8 8 0 1 1 10.5 4a6.2 6.2 0 0 0 9.5 9.5z")
    val Server = icon(
        "server",
        "M5 4h14a1.5 1.5 0 0 1 1.5 1.5v3.5a1.5 1.5 0 0 1-1.5 1.5H5a1.5 1.5 0 0 1-1.5-1.5V5.5A1.5 1.5 0 0 1 5 4z",
        "M5 13.5h14a1.5 1.5 0 0 1 1.5 1.5v3.5a1.5 1.5 0 0 1-1.5 1.5H5a1.5 1.5 0 0 1-1.5-1.5V15a1.5 1.5 0 0 1 1.5-1.5z",
        "M7.5 7.25h.01M7.5 16.75h.01",
    )
    val Download = icon("download", "M12 4v11", "M7.5 10.5L12 15l4.5-4.5", "M5 20h14")
    val Keyboard = icon(
        "keyboard",
        "M4 6h16a1.5 1.5 0 0 1 1.5 1.5v9a1.5 1.5 0 0 1-1.5 1.5H4a1.5 1.5 0 0 1-1.5-1.5v-9A1.5 1.5 0 0 1 4 6z",
        "M6.5 10h.01M10 10h.01M14 10h.01M17.5 10h.01M8 14h8",
    )
    val Pointer = icon("pointer", "M5 3.5l6 15.5l2.3-6.2l6.2-2.3z")
    val Touch = icon(
        "touch",
        "M9.5 12.5V5.75a1.75 1.75 0 0 1 3.5 0V11",
        "M13 10.25a1.75 1.75 0 0 1 3.5 0V12",
        "M16.5 11.5a1.75 1.75 0 0 1 3.5 0V15a6 6 0 0 1-6 6h-1a6 6 0 0 1-5.1-2.85L5.2 14.6a1.6 1.6 0 0 1 2.7-1.7l1.6 2.1",
    )
    val Volume = icon("volume", "M11 5L6.5 9H3.5v6h3l4.5 4z", "M15.5 9a4.5 4.5 0 0 1 0 6", "M18.5 6a8.5 8.5 0 0 1 0 12")
    val VolumeOff = icon("volume-off", "M11 5L6.5 9H3.5v6h3l4.5 4z", "M16 9.5l5 5M21 9.5l-5 5")
    val Monitor = icon(
        "monitor",
        "M4 4.5h16a1.5 1.5 0 0 1 1.5 1.5v9.5a1.5 1.5 0 0 1-1.5 1.5H4a1.5 1.5 0 0 1-1.5-1.5V6A1.5 1.5 0 0 1 4 4.5z",
        "M8.5 20.5h7M12 17v3.5",
    )
    val Clipboard = icon(
        "clipboard",
        "M9 3.5h6v3H9z",
        "M15 5h2a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h2",
        "M9 12.5h6M9 16h4",
    )
    val Activity = icon("activity", "M3 12h4l2.5-7l5 14l2.5-7h4")
    val Power = icon("power", "M12 3v8.5", "M17.7 6.8a8 8 0 1 1-11.4 0")
    val Close = icon("close", "M6 6l12 12M18 6L6 18")
    val Trash = icon("trash", "M4 7h16", "M9.5 7V4.5h5V7", "M6.5 7l.9 12.5a1.5 1.5 0 0 0 1.5 1.5h6.2a1.5 1.5 0 0 0 1.5-1.5L17.5 7")
    val ChevronDown = icon("chevron-down", "M6.5 9.5L12 15l5.5-5.5")
    val Back = icon("back", "M14.5 6L8.5 12l6 6")
    val Check = icon("check", "M5 12.5l4.5 4.5L19 7.5")
}

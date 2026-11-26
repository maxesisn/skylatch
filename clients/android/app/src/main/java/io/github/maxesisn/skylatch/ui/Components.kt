// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Icon
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

@Composable
fun SkyCard(modifier: Modifier = Modifier, dashed: Boolean = false, padding: Dp = 14.dp, content: @Composable ColumnScope.() -> Unit) {
    val p = LocalPalette.current
    val outline = if (dashed) {
        Modifier.drawBehind {
            val w = 1.dp.toPx()
            drawRoundRect(
                color = p.hair.copy(alpha = 1f),
                topLeft = androidx.compose.ui.geometry.Offset(w / 2, w / 2),
                size = androidx.compose.ui.geometry.Size(size.width - w, size.height - w),
                cornerRadius = CornerRadius(10.dp.toPx()),
                style = Stroke(width = w * 1.4f, pathEffect = PathEffect.dashPathEffect(floatArrayOf(6.dp.toPx(), 4.dp.toPx()))),
            )
        }
    } else {
        Modifier.border(1.dp, p.hair, Radii.card)
    }
    Column(
        modifier
            .clip(Radii.card)
            .background(if (dashed) p.surface else p.card)
            .then(outline)
            .padding(padding),
        content = content,
    )
}

@Composable
fun StatusDot(on: Boolean, color: Color, size: Dp = 8.dp) {
    val p = LocalPalette.current
    Box(
        Modifier
            .size(size)
            .clip(CircleShape)
            .then(if (on) Modifier.background(color) else Modifier.border(1.5.dp, p.muted, CircleShape)),
    )
}

@Composable
fun Badge(text: String, accent: Boolean = false) {
    val p = LocalPalette.current
    Text(
        text,
        fontSize = 11.sp,
        lineHeight = 14.sp,
        color = if (accent) p.accent else p.muted,
        fontWeight = FontWeight.Medium,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier
            .clip(Radii.badge)
            .background(if (accent) p.accent.copy(alpha = 0.12f) else p.wash)
            .padding(horizontal = 6.dp, vertical = 2.dp),
    )
}

enum class ButtonKind { PRIMARY, SECONDARY }

@Composable
fun SkyButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    kind: ButtonKind = ButtonKind.PRIMARY,
    icon: ImageVector? = null,
    enabled: Boolean = true,
    height: Dp = 36.dp,
) {
    val p = LocalPalette.current
    val (bg, fg, border) = when (kind) {
        ButtonKind.PRIMARY -> Triple(p.accent, p.onAccent, Color.Transparent)
        ButtonKind.SECONDARY -> Triple(p.card, p.ink, p.hair)
    }
    Row(
        modifier
            .height(height)
            .clip(Radii.control)
            .background(if (enabled) bg else bg.copy(alpha = bg.alpha * 0.45f))
            .border(1.dp, border, Radii.control)
            .clickable(enabled = enabled, onClick = onClick)
            .padding(horizontal = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.Center,
    ) {
        if (icon != null) {
            Icon(icon, null, tint = if (enabled) fg else fg.copy(alpha = 0.5f), modifier = Modifier.size(16.dp))
            Spacer(Modifier.width(6.dp))
        }
        Text(text, color = if (enabled) fg else fg.copy(alpha = 0.6f), fontSize = 14.sp, fontWeight = FontWeight.SemiBold, maxLines = 1)
    }
}

@Composable
fun IconButton(icon: ImageVector, label: String, onClick: () -> Unit, tint: Color? = null, dot: Color? = null) {
    val p = LocalPalette.current
    Box(
        Modifier
            .size(36.dp)
            .clip(Radii.control)
            .clickable(onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Icon(icon, label, tint = tint ?: p.ink, modifier = Modifier.size(20.dp))
        if (dot != null) {
            Box(
                Modifier
                    .align(Alignment.TopEnd)
                    .padding(top = 6.dp, end = 6.dp)
                    .size(8.dp)
                    .clip(CircleShape)
                    .background(p.canvas)
                    .padding(1.5.dp)
                    .clip(CircleShape)
                    .background(dot),
            )
        }
    }
}

@Composable
fun Segmented(options: List<String>, selected: Int, onSelect: (Int) -> Unit, modifier: Modifier = Modifier) {
    val p = LocalPalette.current
    Row(
        modifier
            .height(34.dp)
            .clip(Radii.control)
            .background(p.wash)
            .padding(3.dp),
    ) {
        options.forEachIndexed { i, label ->
            val on = i == selected
            Box(
                Modifier
                    .weight(1f)
                    .height(28.dp)
                    .clip(androidx.compose.foundation.shape.RoundedCornerShape(5.dp))
                    .background(if (on) p.card else Color.Transparent)
                    .clickable { onSelect(i) },
                contentAlignment = Alignment.Center,
            ) {
                Text(label, fontSize = 13.sp, color = if (on) p.ink else p.muted, fontWeight = if (on) FontWeight.SemiBold else FontWeight.Normal)
            }
        }
    }
}

@Composable
fun Field(
    value: String,
    onChange: (String) -> Unit,
    placeholder: String,
    modifier: Modifier = Modifier,
    password: Boolean = false,
    keyboard: KeyboardType = KeyboardType.Text,
) {
    val p = LocalPalette.current
    BasicTextField(
        value = value,
        onValueChange = onChange,
        singleLine = true,
        textStyle = TextStyle(color = p.ink, fontSize = 14.sp),
        cursorBrush = SolidColor(p.accent),
        visualTransformation = if (password) PasswordVisualTransformation() else VisualTransformation.None,
        keyboardOptions = KeyboardOptions(keyboardType = if (password) KeyboardType.Password else keyboard, autoCorrectEnabled = false),
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = 38.dp)
            .clip(Radii.control)
            .background(p.card)
            .border(1.dp, p.hair, Radii.control),
        decorationBox = { inner ->
            Box(Modifier.padding(horizontal = 12.dp, vertical = 10.dp), contentAlignment = Alignment.CenterStart) {
                if (value.isEmpty()) Text(placeholder, color = p.muted.copy(alpha = 0.8f), fontSize = 14.sp, maxLines = 1)
                inner()
            }
        },
    )
}

@Composable
fun SwitchRow(title: String, detail: String?, checked: Boolean, onChange: (Boolean) -> Unit) {
    val p = LocalPalette.current
    Row(
        Modifier
            .fillMaxWidth()
            .clickable { onChange(!checked) }
            .padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(title, color = p.ink, fontSize = 14.sp)
            if (detail != null) Text(detail, color = p.muted, fontSize = 12.sp, lineHeight = 16.sp)
        }
        Switch(
            checked = checked,
            onCheckedChange = onChange,
            colors = SwitchDefaults.colors(
                checkedTrackColor = p.accent,
                checkedThumbColor = p.onAccent,
                uncheckedTrackColor = p.wash,
                uncheckedThumbColor = p.muted,
                uncheckedBorderColor = p.hair,
            ),
        )
    }
}

@Composable
fun SectionTitle(text: String) {
    val p = LocalPalette.current
    Text(text, color = p.muted, fontSize = 12.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.padding(top = 18.dp, bottom = 8.dp))
}

@Composable
fun InfoLine(label: String, value: String) {
    val p = LocalPalette.current
    Row(Modifier.fillMaxWidth().padding(vertical = 3.dp)) {
        Text(label, color = p.muted, fontSize = 13.sp, modifier = Modifier.width(76.dp))
        Text(value, color = p.ink, fontSize = 13.sp, modifier = Modifier.weight(1f))
    }
}

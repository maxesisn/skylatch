// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.ui

import android.app.Activity
import android.graphics.Bitmap
import android.os.Build
import android.view.PointerIcon
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.WindowManager
import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.isImeVisible
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.Shadow
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.layout
import androidx.compose.ui.layout.boundsInParent
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import io.github.maxesisn.skylatch.AppModel
import io.github.maxesisn.skylatch.input.Box as VideoBox
import io.github.maxesisn.skylatch.input.Geometry
import io.github.maxesisn.skylatch.input.InputController
import io.github.maxesisn.skylatch.input.KeyMap
import io.github.maxesisn.skylatch.input.StreamInputView
import io.github.maxesisn.skylatch.stream.HudMode
import io.github.maxesisn.skylatch.stream.HudPlacement
import io.github.maxesisn.skylatch.stream.PerfFormat
import io.github.maxesisn.skylatch.stream.PerfReadout
import io.github.maxesisn.skylatch.stream.RemoteCursor
import io.github.maxesisn.skylatch.stream.StreamSession
import kotlinx.coroutines.delay
import kotlin.math.roundToInt

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun StreamScreen(model: AppModel, session: StreamSession, onInput: (InputController?) -> Unit) {
    val context = LocalContext.current
    val input = remember(session) {
        InputController(session::send, session.scroll, if (session.device.os == "macos") KeyMap.LEFT_GUI else KeyMap.LEFT_CTRL)
    }
    input.trackpad = model.trackpad
    var inputView by remember { mutableStateOf<StreamInputView?>(null) }
    var barOpen by remember { mutableStateOf(model.debugBar) }
    var barTouched by remember { mutableIntStateOf(0) }
    var monitorsOpen by remember { mutableStateOf(false) }
    var hud by remember { mutableStateOf(model.debugHud ?: HudMode.HIDDEN) }
    var readout by remember { mutableStateOf(PerfReadout()) }
    var barRect by remember { mutableStateOf(androidx.compose.ui.geometry.Rect.Zero) }
    val imeVisible = WindowInsets.isImeVisible

    DisposableEffect(session) {
        onInput(input)
        val activity = context as Activity
        val window = activity.window
        val controller = WindowCompat.getInsetsController(window, window.decorView)
        controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        controller.hide(WindowInsetsCompat.Type.systemBars())
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        onDispose {
            onInput(null)
            input.releaseAll()
            controller.show(WindowInsetsCompat.Type.systemBars())
            window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
    }
    LaunchedEffect(session) {
        while (true) {
            readout = session.readout()
            session.tick()
            delay(500)
        }
    }
    LaunchedEffect(barOpen, barTouched, monitorsOpen) {
        if (barOpen && !monitorsOpen && !model.debugBar) {
            delay(6_000)
            barOpen = false
        }
    }
    BackHandler {
        when {
            monitorsOpen -> monitorsOpen = false
            else -> barOpen = !barOpen
        }
    }

    BoxWithConstraints(Modifier.fillMaxSize().background(Color.Black)) {
        val w = constraints.maxWidth.toFloat()
        val h = constraints.maxHeight.toFloat()
        val base = Geometry.letterbox(w, h, session.videoWidth, session.videoHeight)
        input.base = base
        input.videoWidth = session.videoWidth
        input.videoHeight = session.videoHeight
        val shown = if (base.isEmpty) VideoBox(0f, 0f, w, h) else Geometry.zoomed(base, input.zoom)

        VideoSurface(session, shown, w, h)
        AndroidView(
            factory = { StreamInputView(it, input).also { v -> inputView = v; v.requestFocus() } },
            modifier = Modifier.fillMaxSize(),
        )
        if (session.firstFrame) CursorOverlay(session.cursor, input, shown, session.videoWidth)
        PointerIconSync(inputView, session.cursor, shown, session.videoWidth)

        if (hud != HudMode.HIDDEN && session.firstFrame) PerfHud(hud, readout, w, h, if (barOpen) barRect else androidx.compose.ui.geometry.Rect.Zero)

        Latch(
            open = barOpen,
            onOpen = { barOpen = true; barTouched++ },
            modifier = Modifier.align(Alignment.TopCenter).onGloballyPositioned { barRect = it.boundsInParent() },
        ) {
            LatchButton(if (session.audioOn) SkyIcons.Volume else SkyIcons.VolumeOff, if (session.audioOn) "音频" else "已静音", session.audioOn) {
                model.setAudio(!session.audioOn); barTouched++
            }
            LatchButton(if (input.trackpad) SkyIcons.Pointer else SkyIcons.Touch, if (input.trackpad) "触控板" else "直触", true) {
                input.releaseAll()
                model.updateTrackpad(!input.trackpad)
                input.trackpad = model.trackpad
                barTouched++
            }
            LatchButton(SkyIcons.Keyboard, "键盘", imeVisible) {
                if (imeVisible) inputView?.hideKeyboard() else inputView?.showKeyboard()
                barOpen = false
            }
            if (session.monitors.size > 1) {
                LatchButton(SkyIcons.Monitor, "屏幕", monitorsOpen) { monitorsOpen = !monitorsOpen; barTouched++ }
            }
            LatchButton(SkyIcons.Clipboard, "剪贴板", false) {
                session.pushClipboard(true); barTouched++
            }
            LatchButton(SkyIcons.Activity, "数据", hud != HudMode.HIDDEN) { hud = hud.next; barTouched++ }
            LatchButton(SkyIcons.Power, "断开", false, tint = Carbon.danger) { model.leaveStream() }
        }
        if (barOpen && monitorsOpen) {
            MonitorMenu(session, Modifier.align(Alignment.TopCenter).padding(top = 92.dp)) { id ->
                session.selectMonitor(id)
                monitorsOpen = false
                barOpen = false
            }
        }

        if (imeVisible) {
            ShortcutBar(input, session, Modifier.align(Alignment.BottomCenter).imePadding()) { inputView?.hideKeyboard() }
        }

        when (session.phase) {
            StreamSession.Phase.CONNECTING -> StatusPill("正在连接 ${session.device.name}", Modifier.align(Alignment.Center))
            StreamSession.Phase.LIVE -> if (!session.firstFrame) StatusPill("等待首帧画面", Modifier.align(Alignment.Center))
            StreamSession.Phase.CLOSED -> ReconnectCard(session, { session.connect() }, { model.leaveStream() })
        }
    }
}

@Composable
private fun VideoSurface(session: StreamSession, shown: VideoBox, w: Float, h: Float) {
    val vw = session.videoWidth
    val vh = session.videoHeight
    AndroidView(
        factory = { ctx ->
            SurfaceView(ctx).apply {
                holder.addCallback(object : SurfaceHolder.Callback {
                    override fun surfaceCreated(holder: SurfaceHolder) = session.setSurface(holder.surface)
                    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) = Unit
                    override fun surfaceDestroyed(holder: SurfaceHolder) = session.setSurface(null)
                })
            }
        },
        update = { v -> if (vw > 0 && vh > 0) v.holder.setFixedSize(vw, vh) },
        modifier = Modifier.layout { m, _ ->
            val pw = shown.width.roundToInt().coerceAtLeast(1)
            val ph = shown.height.roundToInt().coerceAtLeast(1)
            val placeable = m.measure(Constraints.fixed(pw, ph))
            layout(w.roundToInt(), h.roundToInt()) { placeable.place(shown.left.roundToInt(), shown.top.roundToInt()) }
        },
    )
}

private fun cursorScale(c: RemoteCursor, shown: VideoBox, videoWidth: Int, minPx: Float): Float {
    val s = if (videoWidth > 0) shown.width / videoWidth else 1f
    return maxOf(s, minPx / maxOf(1, c.bitmap.height))
}

@Composable
private fun CursorOverlay(cursor: RemoteCursor?, input: InputController, shown: VideoBox, videoWidth: Int) {
    if (input.usingMouse) return
    if (cursor == null && !input.trackpad) return
    if (cursor != null && !cursor.visible) return
    val image = remember(cursor) { cursor?.bitmap?.asImageBitmap() }
    val arrow = remember { Path() }
    Canvas(Modifier.fillMaxSize()) {
        val x = Geometry.denormalize(input.cursorX, shown.left, shown.width)
        val y = Geometry.denormalize(input.cursorY, shown.top, shown.height)
        if (cursor != null && image != null) {
            val s = cursorScale(cursor, shown, videoWidth, 22.dp.toPx())
            drawImage(
                image,
                dstOffset = IntOffset((x - cursor.hotX * s).roundToInt(), (y - cursor.hotY * s).roundToInt()),
                dstSize = IntSize((image.width * s).roundToInt(), (image.height * s).roundToInt()),
                filterQuality = FilterQuality.Medium,
            )
        } else {
            val u = 1.dp.toPx()
            arrow.reset()
            arrow.moveTo(0f, 0f)
            arrow.lineTo(0f, 17f * u)
            arrow.lineTo(4.6f * u, 13.2f * u)
            arrow.lineTo(7.6f * u, 19.6f * u)
            arrow.lineTo(10.2f * u, 18.4f * u)
            arrow.lineTo(7.3f * u, 12.2f * u)
            arrow.lineTo(13f * u, 12.2f * u)
            arrow.close()
            translate(x, y) {
                drawPath(arrow, Color.White)
                drawPath(arrow, Color.Black, style = Stroke(width = 1.2f * u))
            }
        }
    }
}

/** A hardware mouse shows the host's cursor image as the system pointer, so it tracks without lag. */
@Composable
private fun PointerIconSync(view: StreamInputView?, cursor: RemoteCursor?, shown: VideoBox, videoWidth: Int) {
    val scale = if (videoWidth > 0) shown.width / videoWidth else 1f
    LaunchedEffect(view, cursor, (scale * 20).roundToInt()) {
        val v = view ?: return@LaunchedEffect
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.N) return@LaunchedEffect
        v.pointerIcon = when {
            cursor == null -> null
            !cursor.visible -> PointerIcon.getSystemIcon(v.context, PointerIcon.TYPE_NULL)
            else -> {
                val s = cursorScale(cursor, shown, videoWidth, 0f).coerceAtLeast(0.25f)
                val bmp = Bitmap.createScaledBitmap(
                    cursor.bitmap,
                    (cursor.bitmap.width * s).roundToInt().coerceAtLeast(1),
                    (cursor.bitmap.height * s).roundToInt().coerceAtLeast(1),
                    true,
                )
                PointerIcon.create(bmp, cursor.hotX * s, cursor.hotY * s)
            }
        }
    }
}

@Composable
private fun PerfHud(mode: HudMode, r: PerfReadout, boundsW: Float, boundsH: Float, avoid: androidx.compose.ui.geometry.Rect) {
    val density = LocalDensity.current
    val margin = with(density) { 14.dp.toPx() }
    var x by remember { mutableFloatStateOf(margin) }
    var y by remember { mutableFloatStateOf(margin) }
    var size by remember { mutableStateOf(IntSize.Zero) }
    val (cx, clampedY) = HudPlacement.clamp(x, y, size.width.toFloat(), size.height.toFloat(), boundsW, boundsH)
    val overlaps = !avoid.isEmpty && cx < avoid.right && cx + size.width > avoid.left && clampedY < avoid.bottom
    val cy = if (overlaps) avoid.bottom + margin / 2 else clampedY
    val dot = if (r.isSlow) Carbon.amber else Carbon.green
    val mono = androidx.compose.ui.text.TextStyle(
        fontFamily = FontFamily.Monospace,
        fontSize = 11.sp,
        lineHeight = 15.sp,
        color = Carbon.ink,
        shadow = Shadow(Color.Black.copy(alpha = 0.7f), Offset(0f, 1f), 3f),
    )
    Box(
        Modifier
            .offset { IntOffset(cx.roundToInt(), cy.roundToInt()) }
            .onSizeChanged { size = it }
            .clip(Radii.card)
            .background(Carbon.hud)
            .border(1.dp, Carbon.line, Radii.card)
            .pointerInput(boundsW, boundsH) {
                detectDragGestures { change, drag ->
                    change.consume()
                    val (nx, ny) = HudPlacement.clamp(x + drag.x, y + drag.y, size.width.toFloat(), size.height.toFloat(), boundsW, boundsH)
                    x = nx
                    y = ny
                }
            }
            .padding(horizontal = 10.dp, vertical = 7.dp),
    ) {
        if (mode == HudMode.STRIP) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.size(6.dp).clip(CircleShape).background(dot))
                Spacer(Modifier.width(6.dp))
                Text(PerfFormat.strip(r), style = mono, maxLines = 1)
            }
        } else {
            Column(Modifier.width(IntrinsicSize.Max)) {
                PerfFormat.card(r).forEachIndexed { i, row ->
                    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                        if (i == 0) {
                            Box(Modifier.size(6.dp).clip(CircleShape).background(dot))
                            Spacer(Modifier.width(6.dp))
                        }
                        Text(row.left, style = mono, maxLines = 1)
                        Spacer(Modifier.weight(1f).widthIn(min = 16.dp))
                        Text(row.right, style = mono.copy(color = Carbon.soft), maxLines = 1)
                    }
                }
            }
        }
    }
}

@Composable
private fun Latch(open: Boolean, onOpen: () -> Unit, modifier: Modifier, content: @Composable () -> Unit) {
    Column(modifier.padding(top = 6.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        AnimatedVisibility(!open, enter = fadeIn(), exit = fadeOut()) {
            Box(
                Modifier
                    .size(width = 72.dp, height = 22.dp)
                    .clickable(remember { MutableInteractionSource() }, null, onClick = onOpen)
                    .pointerInput(Unit) { detectVerticalDragGestures { _, dy -> if (dy > 4) onOpen() } },
                contentAlignment = Alignment.TopCenter,
            ) {
                Box(Modifier.padding(top = 4.dp).size(width = 40.dp, height = 4.dp).clip(RoundedCornerShape(2.dp)).background(Color.White.copy(alpha = 0.55f)))
            }
        }
        AnimatedVisibility(open, enter = slideInVertically { -it } + fadeIn(), exit = slideOutVertically { -it } + fadeOut()) {
            Row(
                Modifier
                    .padding(horizontal = 12.dp)
                    .clip(Radii.card)
                    .background(Carbon.glass)
                    .border(1.dp, Carbon.line, Radii.card)
                    .horizontalScroll(rememberScrollState())
                    .padding(horizontal = 6.dp, vertical = 5.dp),
                horizontalArrangement = Arrangement.spacedBy(2.dp),
            ) { content() }
        }
    }
}

@Composable
private fun LatchButton(icon: ImageVector, label: String, active: Boolean, tint: Color? = null, onClick: () -> Unit) {
    Column(
        Modifier
            .width(56.dp)
            .clip(Radii.control)
            .background(if (active && tint == null) Color.White.copy(alpha = 0.08f) else Color.Transparent)
            .clickable(onClick = onClick)
            .padding(vertical = 6.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Icon(icon, label, tint = tint ?: if (active) Carbon.ink else Carbon.muted, modifier = Modifier.size(19.dp))
        Spacer(Modifier.height(3.dp))
        Text(label, color = tint ?: if (active) Carbon.ink else Carbon.muted, fontSize = 10.sp, maxLines = 1)
    }
}

@Composable
private fun MonitorMenu(session: StreamSession, modifier: Modifier, onSelect: (String) -> Unit) {
    Column(
        modifier
            .widthIn(min = 220.dp)
            .clip(Radii.card)
            .background(Carbon.glass)
            .border(1.dp, Carbon.line, Radii.card)
            .padding(6.dp),
    ) {
        session.monitors.forEach { m ->
            Row(
                Modifier
                    .fillMaxWidth()
                    .clip(Radii.control)
                    .background(if (m.active) Color.White.copy(alpha = 0.08f) else Color.Transparent)
                    .clickable { onSelect(m.id) }
                    .padding(horizontal = 10.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Box(Modifier.size(6.dp).clip(CircleShape).background(if (m.active) Carbon.accent else Color.Transparent))
                Spacer(Modifier.width(8.dp))
                Text(m.name.ifBlank { m.id }, color = Carbon.ink, fontSize = 13.sp, modifier = Modifier.weight(1f))
                Text("${m.width}\u00D7${m.height}${if (m.primary) "  主屏" else ""}", color = Carbon.muted, fontSize = 12.sp)
            }
        }
    }
}

@Composable
private fun ShortcutBar(input: InputController, session: StreamSession, modifier: Modifier, onHide: () -> Unit) {
    val gui = if (session.device.os == "macos") "Cmd" else "Win"
    Row(
        modifier
            .fillMaxWidth()
            .background(Carbon.glass)
            .horizontalScroll(rememberScrollState())
            .padding(horizontal = 8.dp, vertical = 6.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        KeyChip("Esc") { input.tap(KeyMap.ESCAPE) }
        KeyChip("Tab") { input.tap(KeyMap.TAB) }
        KeyChip("Ctrl", KeyMap.LEFT_CTRL in input.latched) { input.toggleLatch(KeyMap.LEFT_CTRL) }
        KeyChip("Alt", KeyMap.LEFT_ALT in input.latched) { input.toggleLatch(KeyMap.LEFT_ALT) }
        KeyChip(gui, KeyMap.LEFT_GUI in input.latched) { input.toggleLatch(KeyMap.LEFT_GUI) }
        KeyChip("\u2191") { input.tap(KeyMap.UP) }
        KeyChip("\u2193") { input.tap(KeyMap.DOWN) }
        KeyChip("\u2190") { input.tap(KeyMap.LEFT) }
        KeyChip("\u2192") { input.tap(KeyMap.RIGHT) }
        KeyChip("复制") { input.shortcut('c') }
        KeyChip("粘贴") {
            session.pushClipboard(true)
            android.os.Handler(android.os.Looper.getMainLooper()).postDelayed({ input.shortcut('v') }, 150)
        }
        Box(
            Modifier.size(34.dp).clip(Radii.control).clickable(onClick = onHide),
            contentAlignment = Alignment.Center,
        ) { Icon(SkyIcons.ChevronDown, "收起键盘", tint = Carbon.muted, modifier = Modifier.size(18.dp)) }
    }
}

@Composable
private fun KeyChip(label: String, latched: Boolean = false, onClick: () -> Unit) {
    Text(
        label,
        color = if (latched) Color.White else Carbon.ink,
        fontSize = 13.sp,
        fontWeight = if (latched) FontWeight.SemiBold else FontWeight.Medium,
        modifier = Modifier
            .clip(Radii.control)
            .background(if (latched) Carbon.accent else Color.White.copy(alpha = 0.08f))
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 8.dp),
    )
}

@Composable
private fun StatusPill(text: String, modifier: Modifier) {
    Row(
        modifier
            .clip(Radii.card)
            .background(Carbon.glass)
            .border(1.dp, Carbon.line, Radii.card)
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        CircularProgressIndicator(Modifier.size(16.dp), color = Carbon.accent, strokeWidth = 2.dp)
        Spacer(Modifier.width(10.dp))
        Text(text, color = Carbon.ink, fontSize = 14.sp)
    }
}

@Composable
private fun ReconnectCard(session: StreamSession, onRetry: () -> Unit, onBack: () -> Unit) {
    Box(
        Modifier
            .fillMaxSize()
            .background(Color.Black.copy(alpha = 0.55f))
            .clickable(remember { MutableInteractionSource() }, null) {},
        contentAlignment = Alignment.Center,
    ) {
        Column(
            Modifier
                .padding(24.dp)
                .widthIn(max = 380.dp)
                .clip(Radii.card)
                .background(Color(0xFF1A1C20))
                .border(1.dp, Carbon.line, Radii.card)
                .padding(20.dp),
        ) {
            Text("连接已断开", color = Carbon.ink, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(6.dp))
            Text(session.device.name, color = Carbon.muted, fontSize = 13.sp)
            session.error?.let {
                Spacer(Modifier.height(10.dp))
                Text(
                    it,
                    color = Carbon.danger,
                    fontSize = 13.sp,
                    lineHeight = 18.sp,
                    modifier = Modifier
                        .fillMaxWidth()
                        .clip(Radii.control)
                        .background(Carbon.danger.copy(alpha = 0.1f))
                        .padding(horizontal = 10.dp, vertical = 8.dp),
                )
            }
            Spacer(Modifier.height(18.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                DarkButton("返回设备列表", false, Modifier.weight(1f), onBack)
                DarkButton("重新连接", true, Modifier.weight(1f), onRetry)
            }
        }
    }
}

@Composable
private fun DarkButton(text: String, primary: Boolean, modifier: Modifier, onClick: () -> Unit) {
    Box(
        modifier
            .height(38.dp)
            .clip(Radii.control)
            .background(if (primary) Carbon.accent else Color.White.copy(alpha = 0.08f))
            .clickable(onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, color = Color.White, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
    }
}

// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.github.maxesisn.skylatch.AppModel
import io.github.maxesisn.skylatch.Link
import io.github.maxesisn.skylatch.R
import io.github.maxesisn.skylatch.hub.Device
import io.github.maxesisn.skylatch.hub.HubText
import kotlinx.coroutines.delay

@Composable
fun HomeScreen(model: AppModel, dark: Boolean) {
    val p = LocalPalette.current
    LaunchedEffect(model.configured, model.hubUrl, model.token) {
        while (model.configured) {
            model.refresh()
            delay(5_000)
        }
    }
    val rooms = model.devices.mapNotNull { it.room?.takeIf(String::isNotBlank) }.distinct().sorted()
    val room = model.room.takeIf { it in rooms } ?: ""
    val shown = model.devices.filter { room.isEmpty() || it.room == room }
    var pendingRemove by remember { mutableStateOf<Device?>(null) }

    Box(Modifier.fillMaxSize().background(p.canvas)) {
        Column(Modifier.fillMaxSize().systemBarsPadding()) {
            TopBar(model, dark)
            if (rooms.isNotEmpty()) RoomChips(rooms, room, model::selectRoom)
            BoxWithConstraints(Modifier.fillMaxSize()) {
                when {
                    !model.configured -> Welcome { model.drawerOpen = true }
                    maxWidth >= 600.dp -> TwoPane(model, shown) { pendingRemove = it }
                    else -> DeviceList(model, shown, Modifier.fillMaxSize(), onRemove = { pendingRemove = it })
                }
            }
        }
        ServerDrawer(model)
    }

    pendingRemove?.let { d ->
        AlertDialog(
            onDismissRequest = { pendingRemove = null },
            containerColor = p.card,
            shape = Radii.card,
            title = { Text("移除离线记录", color = p.ink, fontSize = 17.sp, fontWeight = FontWeight.SemiBold) },
            text = { Text("从设备列表移除 ${d.name}。它重新上线时会自动回到列表。", color = p.muted, fontSize = 14.sp) },
            confirmButton = {
                TextButton(onClick = { model.removeDevice(d); pendingRemove = null }) { Text("移除", color = p.warn) }
            },
            dismissButton = { TextButton(onClick = { pendingRemove = null }) { Text("取消", color = p.muted) } },
        )
    }
}

@Composable
private fun TopBar(model: AppModel, dark: Boolean) {
    val p = LocalPalette.current
    Row(
        Modifier.fillMaxWidth().height(56.dp).padding(start = 16.dp, end = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Image(painterResource(R.drawable.ic_logo), null, Modifier.size(28.dp))
        Spacer(Modifier.width(10.dp))
        Text("Skylatch", color = p.ink, fontSize = 18.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.weight(1f))
        if (model.availableUpdate != null) {
            IconButton(SkyIcons.Download, "有新版本", { model.drawerOpen = true }, tint = p.accent, dot = p.accent)
        }
        IconButton(if (dark) SkyIcons.Sun else SkyIcons.Moon, "切换浅色或深色", { model.setDarkMode(if (dark) 0 else 1) })
        val dot = when (model.link) {
            Link.OK -> p.green
            Link.ERROR -> p.warn
            Link.CHECKING -> p.muted
            Link.NONE -> null
        }
        IconButton(SkyIcons.Server, "服务器与账号", { model.drawerOpen = true }, dot = dot)
    }
}

@Composable
private fun RoomChips(rooms: List<String>, selected: String, onSelect: (String) -> Unit) {
    val p = LocalPalette.current
    Row(
        Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        (listOf("") + rooms).forEach { r ->
            val on = r == selected
            Text(
                if (r.isEmpty()) "全部" else r,
                fontSize = 13.sp,
                color = if (on) p.onAccent else p.ink,
                fontWeight = if (on) FontWeight.SemiBold else FontWeight.Normal,
                modifier = Modifier
                    .clip(Radii.control)
                    .background(if (on) p.accent else p.card)
                    .border(1.dp, if (on) Color.Transparent else p.hair, Radii.control)
                    .clickable { onSelect(r) }
                    .padding(horizontal = 12.dp, vertical = 6.dp),
            )
        }
    }
}

@Composable
private fun Welcome(onSetup: () -> Unit) {
    val p = LocalPalette.current
    Box(Modifier.fillMaxSize().padding(24.dp), contentAlignment = Alignment.Center) {
        SkyCard(Modifier.widthIn(max = 420.dp), padding = 22.dp) {
            Image(painterResource(R.drawable.ic_logo), null, Modifier.size(44.dp))
            Spacer(Modifier.height(14.dp))
            Text("连接到你的 Skylatch Hub", color = p.ink, fontSize = 18.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(6.dp))
            Text(
                "填写 Hub 地址并登录后，这里会列出账号下的所有电脑，轻点即可接管屏幕。",
                color = p.muted,
                fontSize = 14.sp,
                lineHeight = 20.sp,
            )
            Spacer(Modifier.height(18.dp))
            SkyButton("设置服务器与账号", onSetup, Modifier.fillMaxWidth(), icon = SkyIcons.Server)
        }
    }
}

@Composable
private fun DeviceList(
    model: AppModel,
    devices: List<Device>,
    modifier: Modifier,
    selected: String? = null,
    onSelect: ((Device) -> Unit)? = null,
    onRemove: (Device) -> Unit,
) {
    val p = LocalPalette.current
    LazyColumn(modifier, contentPadding = PaddingValues(16.dp, 8.dp, 16.dp, 24.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        if (model.devicesError != null) {
            item { Notice(model.devicesError!!) }
        }
        if (model.loaded && devices.isEmpty()) {
            item {
                Text(
                    "账号下还没有在线或待机的电脑。在电脑上安装 Skylatch 并登录同一账号后会出现在这里。",
                    color = p.muted, fontSize = 14.sp, lineHeight = 20.sp, textAlign = TextAlign.Center,
                    modifier = Modifier.fillMaxWidth().padding(24.dp),
                )
            }
        }
        items(devices, key = { it.id }) { d ->
            DeviceCard(
                d,
                selected = d.id == selected,
                onClick = onSelect?.let { { it(d) } },
                onConnect = { model.connect(d) },
                onRemove = { onRemove(d) },
                compact = onSelect != null,
            )
        }
    }
}

@Composable
fun Notice(text: String) {
    val p = LocalPalette.current
    Text(
        text,
        color = p.warn,
        fontSize = 13.sp,
        modifier = Modifier
            .fillMaxWidth()
            .clip(Radii.control)
            .background(p.warn.copy(alpha = 0.10f))
            .padding(horizontal = 12.dp, vertical = 8.dp),
    )
}

@Composable
private fun DeviceCard(
    d: Device,
    selected: Boolean,
    onClick: (() -> Unit)?,
    onConnect: () -> Unit,
    onRemove: () -> Unit,
    compact: Boolean,
) {
    val p = LocalPalette.current
    val now = System.currentTimeMillis() / 1000
    SkyCard(
        Modifier
            .fillMaxWidth()
            .then(if (selected) Modifier.border(1.5.dp, p.accent, Radii.card) else Modifier)
            .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier),
        dashed = !d.online,
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            StatusDot(d.online, p.green)
            Spacer(Modifier.width(10.dp))
            Text(
                d.name,
                color = if (d.online) p.ink else p.muted,
                fontSize = 16.sp,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            d.room?.let {
                Spacer(Modifier.width(8.dp))
                Badge(it)
            }
        }
        Spacer(Modifier.height(6.dp))
        Row(Modifier.padding(start = 18.dp), verticalAlignment = Alignment.CenterVertically) {
            Badge(d.hardwareModel ?: HubText.formFactor(d.formFactor))
            Spacer(Modifier.width(8.dp))
            Text(HubText.summary(d), color = p.muted, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        if (compact) {
            if (!d.online) {
                Text(HubText.lastSeen(d.lastSeenUnix, now).ifEmpty { "离线待机" }, color = p.muted, fontSize = 12.sp, modifier = Modifier.padding(start = 18.dp, top = 2.dp))
            }
            return@SkyCard
        }
        Spacer(Modifier.height(12.dp))
        if (d.online) {
            SkyButton("接管屏幕", onConnect, Modifier.fillMaxWidth(), icon = SkyIcons.Monitor)
        } else {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    HubText.lastSeen(d.lastSeenUnix, now).ifEmpty { "离线待机" },
                    color = p.muted,
                    fontSize = 13.sp,
                    modifier = Modifier.weight(1f).padding(start = 18.dp),
                )
                SkyButton("移除离线记录", onRemove, kind = ButtonKind.SECONDARY, icon = SkyIcons.Trash, height = 32.dp)
            }
        }
    }
}

@Composable
private fun TwoPane(model: AppModel, devices: List<Device>, onRemove: (Device) -> Unit) {
    var selectedId by rememberSaveable { mutableStateOf<String?>(null) }
    val selected = devices.firstOrNull { it.id == selectedId } ?: devices.firstOrNull()
    Row(Modifier.fillMaxSize()) {
        DeviceList(
            model, devices,
            Modifier.widthIn(min = 300.dp, max = 400.dp).fillMaxHeight(),
            selected = selected?.id,
            onSelect = { selectedId = it.id },
            onRemove = onRemove,
        )
        Box(Modifier.weight(1f).fillMaxHeight().padding(top = 8.dp, end = 16.dp, bottom = 24.dp)) {
            if (selected != null) DeviceDetail(selected, { model.connect(selected) }, { onRemove(selected) })
        }
    }
}

@Composable
private fun DeviceDetail(d: Device, onConnect: () -> Unit, onRemove: () -> Unit) {
    val p = LocalPalette.current
    val now = System.currentTimeMillis() / 1000
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
        SkyCard(Modifier.fillMaxWidth(), dashed = !d.online, padding = 20.dp) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                StatusDot(d.online, p.green, 10.dp)
                Spacer(Modifier.width(10.dp))
                Text(d.name, color = p.ink, fontSize = 22.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            Spacer(Modifier.height(8.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.padding(start = 20.dp)) {
                Badge(if (d.online) "在线" else "离线待机", accent = d.online)
                Badge(HubText.os(d.os))
                Badge(HubText.formFactor(d.formFactor))
                d.room?.let { Badge(it) }
            }
            Spacer(Modifier.height(16.dp))
            d.hardwareModel?.let { InfoLine("型号", it) }
            InfoLine("系统", HubText.os(d.os))
            if (d.hostVersion.isNotBlank()) InfoLine("主机版本", d.hostVersion)
            d.slot?.let { InfoLine("工位", "$it 号") }
            InfoLine("屏幕", HubText.screens(d.screenCount))
            d.monitorList.filter { it.width > 0 }.forEach { m ->
                InfoLine("", "${m.name.ifBlank { m.id }}  ${m.width}\u00D7${m.height}${if (m.primary) "  主屏" else ""}")
            }
            InfoLine("直连地址", if (d.directEndpoints > 0) "${d.directEndpoints} 个" else "无")
            if (!d.online) InfoLine("状态", HubText.lastSeen(d.lastSeenUnix, now).ifEmpty { "离线待机" })
            Spacer(Modifier.height(20.dp))
            if (d.online) {
                SkyButton("接管屏幕", onConnect, Modifier.fillMaxWidth(), icon = SkyIcons.Monitor, height = 40.dp)
            } else {
                SkyButton("移除离线记录", onRemove, Modifier.fillMaxWidth(), kind = ButtonKind.SECONDARY, icon = SkyIcons.Trash, height = 40.dp)
            }
        }
    }
}

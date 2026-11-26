// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.ui

import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.github.maxesisn.skylatch.AppModel
import io.github.maxesisn.skylatch.BuildConfig
import io.github.maxesisn.skylatch.Link
import io.github.maxesisn.skylatch.UpdateState
import io.github.maxesisn.skylatch.hub.HubText
import kotlinx.coroutines.delay

/** Server and account settings: a bottom sheet on phones, a right-hand drawer from 600dp. */
@Composable
fun ServerDrawer(model: AppModel) {
    val p = LocalPalette.current
    BackHandler(model.drawerOpen) { model.drawerOpen = false }
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val wide = maxWidth >= 600.dp
        AnimatedVisibility(model.drawerOpen, enter = fadeIn(), exit = fadeOut()) {
            Box(
                Modifier
                    .fillMaxSize()
                    .background(Color.Black.copy(alpha = if (p.dark) 0.5f else 0.28f))
                    .clickable(remember { MutableInteractionSource() }, null) { model.drawerOpen = false },
            )
        }
        AnimatedVisibility(
            model.drawerOpen,
            modifier = Modifier.align(if (wide) Alignment.CenterEnd else Alignment.BottomCenter),
            enter = if (wide) slideInHorizontally { it } else slideInVertically { it },
            exit = if (wide) slideOutHorizontally { it } else slideOutVertically { it },
        ) {
            val shape = if (wide) RoundedCornerShape(topStart = 10.dp, bottomStart = 10.dp) else RoundedCornerShape(topStart = 10.dp, topEnd = 10.dp)
            Column(
                Modifier
                    .then(if (wide) Modifier.width(400.dp).fillMaxHeight() else Modifier.fillMaxWidth().heightIn(max = maxHeight * 0.92f))
                    .clip(shape)
                    .background(p.surface)
                    .clickable(remember { MutableInteractionSource() }, null) {}
                    .then(if (wide) Modifier.statusBarsPadding() else Modifier)
                    .navigationBarsPadding()
                    .imePadding(),
            ) {
                DrawerContent(model)
            }
        }
    }
}

@Composable
private fun DrawerContent(model: AppModel) {
    val p = LocalPalette.current
    var url by remember { mutableStateOf(model.hubUrl) }
    var tab by remember { mutableIntStateOf(0) }
    var username by remember { mutableStateOf("") }
    var password by remember { mutableStateOf("") }
    var token by remember { mutableStateOf("") }

    LaunchedEffect(url) {
        delay(450)
        model.loadHubInfo(url)
    }
    val info = model.hubInfo?.takeIf { model.hubInfoUrl == HubText.normalizeHubUrl(url) }
    val tabs = buildList {
        add("登录")
        if (info?.allowSignup == true) add("注册")
        add("Token")
    }
    if (tab >= tabs.size) tab = 0

    Row(Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp, top = 10.dp), verticalAlignment = Alignment.CenterVertically) {
        Text("服务器与账号", color = p.ink, fontSize = 17.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
        IconButton(SkyIcons.Close, "关闭", { model.drawerOpen = false }, tint = p.muted)
    }
    Column(Modifier.verticalScroll(rememberScrollState()).padding(start = 20.dp, end = 20.dp, bottom = 24.dp)) {
        SectionTitle("服务器")
        Field(url, { url = it }, "wss://hub.example.com", keyboard = KeyboardType.Uri)
        Spacer(Modifier.height(10.dp))
        SkyCard(Modifier.fillMaxWidth(), padding = 12.dp) {
            when {
                info != null -> {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        StatusDot(true, p.green)
                        Spacer(Modifier.width(8.dp))
                        Text(HubText.modeLabel(info.mode), color = p.ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                        Spacer(Modifier.weight(1f))
                        Badge("Hub ${info.version}")
                    }
                    Spacer(Modifier.height(6.dp))
                    Text(HubText.modeDetail(info.mode), color = p.muted, fontSize = 12.sp, lineHeight = 17.sp)
                    Text(HubText.peersLine(info.easytierPeers), color = p.muted, fontSize = 12.sp, lineHeight = 17.sp)
                    Row(Modifier.padding(top = 8.dp), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        Badge(if (info.allowSignup) "开放注册" else "仅限已有账号")
                        info.maxDevicesPerAccount?.let { Badge("每账号 $it 台") }
                    }
                }
                model.hubInfoError != null && model.hubInfoUrl == HubText.normalizeHubUrl(url) -> {
                    Text("无法读取 Hub 信息", color = p.warn, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                    Text(model.hubInfoError!!, color = p.muted, fontSize = 12.sp, lineHeight = 17.sp)
                }
                HubText.hubUrlValid(HubText.normalizeHubUrl(url)) -> Text("正在读取 Hub 信息", color = p.muted, fontSize = 13.sp)
                else -> Text("填写 Hub 地址，例如 wss://hub.example.com", color = p.muted, fontSize = 13.sp)
            }
        }

        SectionTitle("账号")
        if (model.token.isNotEmpty()) {
            SkyCard(Modifier.fillMaxWidth(), padding = 12.dp) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    StatusDot(model.link == Link.OK, if (model.link == Link.ERROR) p.warn else p.green)
                    Spacer(Modifier.width(8.dp))
                    Column(Modifier.weight(1f)) {
                        Text(model.account.ifBlank { "Token 登录" }, color = p.ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                        Text(
                            when (model.link) {
                                Link.OK -> "已连接 ${model.hubUrl.removePrefix("wss://").removePrefix("ws://")}"
                                Link.ERROR -> model.devicesError ?: "连接失败"
                                else -> "正在连接"
                            },
                            color = if (model.link == Link.ERROR) p.warn else p.muted,
                            fontSize = 12.sp,
                            maxLines = 2,
                        )
                    }
                    SkyButton("退出", { model.signOut() }, kind = ButtonKind.SECONDARY, height = 30.dp)
                }
            }
            Spacer(Modifier.height(10.dp))
        }
        Segmented(tabs, tab, { tab = it }, Modifier.fillMaxWidth())
        Spacer(Modifier.height(10.dp))
        when (tabs[tab]) {
            "Token" -> {
                Field(token, { token = it }, "账号 Token", password = true)
                Spacer(Modifier.height(10.dp))
                SkyButton("使用 Token", { model.useToken(url, token); token = "" }, Modifier.fillMaxWidth())
            }
            else -> {
                val signup = tabs[tab] == "注册"
                Field(username, { username = it }, "用户名")
                Spacer(Modifier.height(8.dp))
                Field(password, { password = it }, "密码", password = true)
                Spacer(Modifier.height(10.dp))
                SkyButton(
                    if (model.busy) "请稍候" else if (signup) "注册并登录" else "登录",
                    { model.signIn(url, username, password, signup); password = "" },
                    Modifier.fillMaxWidth(),
                    enabled = !model.busy,
                )
            }
        }
        model.accountError?.let {
            Spacer(Modifier.height(8.dp))
            Notice(it)
        }

        SectionTitle("连接")
        SwitchRow("优先直连", "能打洞直达时绕开 Hub 中继，延迟更低", model.preferDirect, model::updatePreferDirect)
        SwitchRow("触控板模式", "单指移动光标，轻点单击；关闭后为直接触控", model.trackpad, model::updateTrackpad)
        SwitchRow("回到前台时发送剪贴板", "切回 Skylatch 时把本机剪贴板同步给远端", model.clipboardOnResume, model::updateClipboardOnResume)

        SectionTitle("关于")
        UpdateCard(model)
    }
}

@Composable
private fun UpdateCard(model: AppModel) {
    val p = LocalPalette.current
    SkyCard(Modifier.fillMaxWidth(), padding = 12.dp) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("Skylatch Android ${BuildConfig.VERSION_NAME}", color = p.ink, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                Text(
                    when (val u = model.update) {
                        UpdateState.Idle -> "从 GitHub Releases 检查新版本"
                        UpdateState.Checking -> "正在检查更新"
                        is UpdateState.Latest -> "已是最新版本"
                        is UpdateState.Available -> "发现新版本 ${u.info.version}"
                        is UpdateState.Downloading -> "正在下载 ${u.info.version}"
                        is UpdateState.Ready -> "${u.info.version} 已下载并校验"
                        is UpdateState.Failed -> u.message
                    },
                    color = if (model.update is UpdateState.Failed) p.warn else p.muted,
                    fontSize = 12.sp,
                    maxLines = 2,
                )
            }
            Spacer(Modifier.width(8.dp))
            when (val u = model.update) {
                is UpdateState.Available -> SkyButton("下载并安装", { model.downloadUpdate() }, icon = SkyIcons.Download, height = 32.dp)
                is UpdateState.Ready -> SkyButton("安装", { model.installUpdate() }, height = 32.dp)
                is UpdateState.Downloading, UpdateState.Checking -> Unit
                else -> if (u !is UpdateState.Downloading) SkyButton("检查更新", { model.checkUpdate(false) }, kind = ButtonKind.SECONDARY, icon = SkyIcons.Refresh, height = 32.dp)
            }
        }
        val u = model.update
        if (u is UpdateState.Downloading) {
            Spacer(Modifier.height(10.dp))
            if (u.total > 0) {
                LinearProgressIndicator(
                    progress = { (u.done.toFloat() / u.total).coerceIn(0f, 1f) },
                    modifier = Modifier.fillMaxWidth().height(4.dp).clip(Radii.badge),
                    color = p.accent,
                    trackColor = p.wash,
                )
            } else {
                LinearProgressIndicator(Modifier.fillMaxWidth().height(4.dp).clip(Radii.badge), color = p.accent, trackColor = p.wash)
            }
        }
        if (u is UpdateState.Available && !u.info.notes.isNullOrBlank()) {
            Spacer(Modifier.height(8.dp))
            Text(u.info.notes!!.take(400), color = p.muted, fontSize = 12.sp, lineHeight = 17.sp, maxLines = 6)
        }
    }
    Spacer(Modifier.size(4.dp))
}

// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch

import android.content.Context
import android.os.Handler
import android.os.Looper
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import io.github.maxesisn.skylatch.hub.Device
import io.github.maxesisn.skylatch.hub.DownloadedApk
import io.github.maxesisn.skylatch.hub.HubInfo
import io.github.maxesisn.skylatch.hub.HubJson
import io.github.maxesisn.skylatch.hub.HubText
import io.github.maxesisn.skylatch.hub.Settings
import io.github.maxesisn.skylatch.hub.UpdateInfo
import io.github.maxesisn.skylatch.stream.StreamSession
import io.github.maxesisn.skylatch.update.Updater
import java.util.concurrent.Executors

enum class Link { NONE, CHECKING, OK, ERROR }

sealed interface UpdateState {
    data object Idle : UpdateState
    data object Checking : UpdateState
    data class Latest(val version: String) : UpdateState
    data class Available(val info: UpdateInfo) : UpdateState
    data class Downloading(val info: UpdateInfo, val done: Long, val total: Long) : UpdateState
    data class Ready(val info: UpdateInfo, val apk: DownloadedApk) : UpdateState
    data class Failed(val message: String) : UpdateState
}

/** Everything the app shows outside a stream, with the hub calls run off the main thread. */
class AppModel(private val context: Context) {
    val settings = Settings(context)
    val io = Executors.newCachedThreadPool()
    private val main = Handler(Looper.getMainLooper())

    var hubUrl by mutableStateOf(settings.hubUrl)
        private set
    var token by mutableStateOf(settings.token)
        private set
    var account by mutableStateOf(settings.accountName)
        private set
    var dark by mutableStateOf(settings.darkMode)
        private set
    var preferDirect by mutableStateOf(settings.preferDirect)
        private set
    var trackpad by mutableStateOf(settings.trackpad)
        private set
    var clipboardOnResume by mutableStateOf(settings.clipboardOnResume)
        private set
    var room by mutableStateOf(settings.room)
        private set

    var hubInfo by mutableStateOf<HubInfo?>(null)
        private set
    var hubInfoUrl by mutableStateOf("")
        private set
    var hubInfoError by mutableStateOf<String?>(null)
        private set
    var devices by mutableStateOf<List<Device>>(emptyList())
        private set
    var devicesError by mutableStateOf<String?>(null)
        private set
    var link by mutableStateOf(Link.NONE)
        private set
    var loaded by mutableStateOf(false)
        private set
    var busy by mutableStateOf(false)
        private set
    var accountError by mutableStateOf<String?>(null)
        private set
    var update by mutableStateOf<UpdateState>(UpdateState.Idle)
        private set
    var stream by mutableStateOf<StreamSession?>(null)
        private set
    var drawerOpen by mutableStateOf(false)

    /** Development hooks set from launch extras in debug builds. */
    var pendingConnect: String? = null
    var debugHud: io.github.maxesisn.skylatch.stream.HudMode? = null
    var debugBar = false

    val configured: Boolean get() = HubText.hubUrlValid(hubUrl) && token.isNotEmpty()

    private var refreshing = false

    private fun <T> background(work: () -> T, done: (Result<T>) -> Unit) {
        io.execute {
            val r = runCatching(work)
            main.post { done(r) }
        }
    }

    fun setDarkMode(mode: Int) {
        dark = mode
        settings.darkMode = mode
    }

    fun updatePreferDirect(v: Boolean) {
        preferDirect = v
        settings.preferDirect = v
    }

    fun updateTrackpad(v: Boolean) {
        trackpad = v
        settings.trackpad = v
    }

    fun updateClipboardOnResume(v: Boolean) {
        clipboardOnResume = v
        settings.clipboardOnResume = v
    }

    fun selectRoom(r: String) {
        room = r
        settings.room = r
    }

    fun loadHubInfo(input: String) {
        val url = HubText.normalizeHubUrl(input)
        if (!HubText.hubUrlValid(url) || (url == hubInfoUrl && hubInfo != null)) return
        hubInfoUrl = url
        hubInfoError = null
        hubInfo = null
        background({ HubJson.hubInfo(NativeBridge.nativeHubInfo(url)) }) { r ->
            if (hubInfoUrl != url) return@background
            r.onSuccess { hubInfo = it }.onFailure { hubInfoError = it.message }
        }
    }

    /** Password sign-in or signup; only the returned session token is kept. */
    fun signIn(input: String, username: String, password: String, signup: Boolean) {
        val url = HubText.normalizeHubUrl(input)
        if (!HubText.hubUrlValid(url)) return run { accountError = "Hub 地址无效" }
        if (username.isBlank() || password.isEmpty()) return run { accountError = "请填写用户名和密码" }
        busy = true
        accountError = null
        background({
            HubJson.login(
                if (signup) NativeBridge.nativeSignup(url, username.trim(), password)
                else NativeBridge.nativeLogin(url, username.trim(), password),
            )
        }) { r ->
            busy = false
            r.onSuccess { saveAccount(url, it.token, it.account) }.onFailure { accountError = it.message }
        }
    }

    fun useToken(input: String, value: String) {
        val url = HubText.normalizeHubUrl(input)
        if (!HubText.hubUrlValid(url)) return run { accountError = "Hub 地址无效" }
        if (value.isBlank()) return run { accountError = "请填写 Token" }
        accountError = null
        saveAccount(url, value.trim(), "")
    }

    private fun saveAccount(url: String, t: String, name: String) {
        hubUrl = url
        token = t
        account = name
        settings.hubUrl = url
        settings.token = t
        settings.accountName = name
        devices = emptyList()
        loaded = false
        loadHubInfo(url)
        refresh()
    }

    fun signOut() {
        token = ""
        account = ""
        settings.token = ""
        settings.accountName = ""
        devices = emptyList()
        loaded = false
        link = Link.NONE
    }

    fun refresh() {
        if (!configured || refreshing) return
        refreshing = true
        val url = hubUrl
        val t = token
        if (!loaded) link = Link.CHECKING
        background({ HubJson.devices(NativeBridge.nativeListDevices(url, t)) }) { r ->
            refreshing = false
            if (url != hubUrl || t != token) return@background
            loaded = true
            r.onSuccess { list ->
                devices = list
                devicesError = null
                link = Link.OK
                pendingConnect?.let { id -> list.firstOrNull { it.id == id && it.online } }?.let {
                    pendingConnect = null
                    if (stream == null) connect(it)
                }
            }.onFailure {
                devicesError = it.message
                link = Link.ERROR
            }
        }
    }

    fun removeDevice(d: Device) {
        val url = hubUrl
        val t = token
        background({ HubJson.ok(NativeBridge.nativeDeleteDevice(url, t, d.id)) }) { r ->
            r.onSuccess { devices = devices.filterNot { it.id == d.id } }.onFailure { devicesError = it.message }
            refresh()
        }
    }

    fun checkUpdate(quiet: Boolean) {
        if (update is UpdateState.Checking || update is UpdateState.Downloading) return
        if (!quiet) update = UpdateState.Checking
        background({ Updater.check() }) { r ->
            r.onSuccess { update = if (it.available) UpdateState.Available(it) else if (quiet) UpdateState.Idle else UpdateState.Latest(it.version) }
                .onFailure { if (!quiet) update = UpdateState.Failed(it.message ?: "检查失败") }
        }
    }

    fun downloadUpdate() {
        val info = (update as? UpdateState.Available)?.info ?: return
        update = UpdateState.Downloading(info, 0, info.size ?: -1)
        background({
            Updater.download(context, info) { done, total -> main.post { update = UpdateState.Downloading(info, done, total) } }
        }) { r ->
            r.onSuccess {
                update = UpdateState.Ready(info, it)
                Updater.install(context, it)
            }.onFailure { update = UpdateState.Failed(it.message ?: "下载失败") }
        }
    }

    fun installUpdate() {
        (update as? UpdateState.Ready)?.let { Updater.install(context, it.apk) }
    }

    val availableUpdate: UpdateInfo?
        get() = when (val u = update) {
            is UpdateState.Available -> u.info
            is UpdateState.Downloading -> u.info
            is UpdateState.Ready -> u.info
            else -> null
        }

    fun connect(d: Device) {
        if (!d.online || !configured) return
        stream?.close()
        stream = StreamSession(context, d, hubUrl, token, preferDirect, settings.audio, io).also { it.connect() }
    }

    fun setAudio(on: Boolean) {
        settings.audio = on
        stream?.setAudio(on)
    }

    fun leaveStream() {
        stream?.close()
        stream = null
        refresh()
    }
}

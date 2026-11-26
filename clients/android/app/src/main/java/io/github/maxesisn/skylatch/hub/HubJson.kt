// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.hub

import org.json.JSONArray
import org.json.JSONException
import org.json.JSONObject

data class Monitor(
    val id: String,
    val name: String,
    val x: Int,
    val y: Int,
    val width: Int,
    val height: Int,
    val primary: Boolean,
    val active: Boolean,
)

data class Device(
    val id: String,
    val name: String,
    val os: String,
    val hostVersion: String,
    val monitors: Int,
    val monitorList: List<Monitor>,
    val formFactor: String,
    val room: String?,
    val slot: Long?,
    val hardwareModel: String?,
    val online: Boolean,
    val lastSeenUnix: Long?,
    val directEndpoints: Int,
) {
    /** The primary display's size, else the first with a size. */
    val resolution: Pair<Int, Int>?
        get() = (monitorList.firstOrNull { it.primary && it.width > 0 } ?: monitorList.firstOrNull { it.width > 0 })
            ?.let { it.width to it.height }

    val screenCount: Int get() = maxOf(monitors, monitorList.count { it.width > 0 }, 1)
}

data class HubInfo(
    val version: String,
    val mode: String,
    val easytierPeers: List<String>,
    val allowSignup: Boolean,
    val maxDevicesPerAccount: Int?,
)

data class Login(val token: String, val account: String)

data class UpdateInfo(
    val available: Boolean,
    val version: String,
    val assetName: String? = null,
    val size: Long? = null,
    val downloadUrl: String? = null,
    val checksumsUrl: String? = null,
    val releaseUrl: String? = null,
    val notes: String? = null,
)

data class DownloadedApk(val path: String, val sha256: String, val version: String)

class HubException(message: String) : Exception(message)

/** Parses the bridge's `{"ok":true,...}` / `{"ok":false,"error":...}` replies. */
object HubJson {
    fun envelope(text: String?): JSONObject {
        val obj = try {
            JSONObject(text ?: throw HubException("native bridge returned nothing"))
        } catch (e: JSONException) {
            throw HubException("unreadable reply: ${e.message}")
        }
        if (!obj.optBoolean("ok", false)) throw HubException(obj.optString("error").ifBlank { "unknown error" })
        return obj
    }

    fun ok(text: String?) {
        envelope(text)
    }

    fun hubInfo(text: String?): HubInfo {
        val d = envelope(text).getJSONObject("data")
        return HubInfo(
            version = d.optString("version"),
            mode = d.optString("mode"),
            easytierPeers = strings(d.optJSONArray("easytier_peers")),
            allowSignup = d.optBoolean("allow_signup", false),
            maxDevicesPerAccount = if (d.has("max_devices_per_account") && !d.isNull("max_devices_per_account")) d.getInt("max_devices_per_account") else null,
        )
    }

    fun login(text: String?): Login {
        val o = envelope(text)
        return Login(o.getString("token"), o.optString("account"))
    }

    fun devices(text: String?): List<Device> {
        val arr = envelope(text).optJSONArray("devices") ?: return emptyList()
        return (0 until arr.length()).map { device(arr.getJSONObject(it)) }
    }

    fun device(o: JSONObject): Device = Device(
        id = o.getString("device_id"),
        name = o.optString("name").ifBlank { o.getString("device_id") },
        os = o.optString("os"),
        hostVersion = o.optString("host_version"),
        monitors = o.optInt("monitors", 1),
        monitorList = monitors(o.optJSONArray("monitor_list")),
        formFactor = o.optString("form_factor").ifBlank { "desktop" },
        room = optText(o, "room"),
        slot = if (o.has("slot") && !o.isNull("slot")) o.getLong("slot") else null,
        hardwareModel = optText(o, "hardware_model"),
        online = o.optBoolean("online", true),
        lastSeenUnix = if (o.has("last_seen_unix") && !o.isNull("last_seen_unix")) o.getLong("last_seen_unix") else null,
        directEndpoints = o.optJSONArray("direct_endpoints")?.length() ?: 0,
    )

    fun monitors(text: String): List<Monitor> = try {
        monitors(JSONArray(text))
    } catch (_: JSONException) {
        emptyList()
    }

    fun monitors(arr: JSONArray?): List<Monitor> {
        if (arr == null) return emptyList()
        return (0 until arr.length()).map {
            val m = arr.getJSONObject(it)
            Monitor(
                id = m.optString("id"),
                name = m.optString("name"),
                x = m.optInt("x"),
                y = m.optInt("y"),
                width = m.optInt("width"),
                height = m.optInt("height"),
                primary = m.optBoolean("primary"),
                active = m.optBoolean("active"),
            )
        }
    }

    fun update(text: String?): UpdateInfo {
        val o = envelope(text)
        return UpdateInfo(
            available = o.optBoolean("available"),
            version = o.optString("version"),
            assetName = optText(o, "asset_name"),
            size = if (o.has("size") && !o.isNull("size")) o.getLong("size") else null,
            downloadUrl = optText(o, "download_url"),
            checksumsUrl = optText(o, "checksums_url"),
            releaseUrl = optText(o, "release_url"),
            notes = optText(o, "notes"),
        )
    }

    fun download(text: String?): DownloadedApk {
        val o = envelope(text)
        return DownloadedApk(o.getString("path"), o.optString("sha256"), o.optString("version"))
    }

    private fun optText(o: JSONObject, key: String): String? =
        if (o.has(key) && !o.isNull(key)) o.optString(key).ifBlank { null } else null

    private fun strings(arr: JSONArray?): List<String> =
        if (arr == null) emptyList() else (0 until arr.length()).map { arr.getString(it) }
}

/** Display text shared by the device list and the server drawer. */
object HubText {
    /** `https://` becomes `wss://`, `http://` becomes `ws://`, a bare host defaults to `wss://`. */
    fun normalizeHubUrl(input: String): String {
        val s = input.trim()
        val url = when {
            s.startsWith("https://") -> "wss://" + s.removePrefix("https://")
            s.startsWith("http://") -> "ws://" + s.removePrefix("http://")
            s.startsWith("wss://") || s.startsWith("ws://") -> s
            s.isEmpty() -> ""
            else -> "wss://$s"
        }
        return url.trimEnd('/')
    }

    fun hubUrlValid(url: String): Boolean {
        val rest = url.removePrefix("wss://").takeIf { it != url } ?: url.removePrefix("ws://").takeIf { it != url } ?: return false
        return rest.trim('/').isNotEmpty()
    }

    fun modeLabel(mode: String): String = when (mode) {
        "relay" -> "直连 + 中继"
        "signaling-only" -> "纯信令模式"
        else -> "未知模式"
    }

    fun modeDetail(mode: String): String = when (mode) {
        "relay" -> "优先直连，打洞失败时经 Hub 中继画面"
        "signaling-only" -> "Hub 只做信令，画面须经直连或虚拟组网"
        else -> "Hub 未声明传输模式"
    }

    fun peersLine(peers: List<String>): String =
        if (peers.isEmpty()) "Hub 未声明配套打洞节点" else "配套打洞节点：" + peers.joinToString("，")

    fun lastSeen(last: Long?, now: Long): String {
        if (last == null) return ""
        val ago = (now - last).coerceAtLeast(0)
        val text = when {
            ago < 60 -> "刚刚"
            ago < 3600 -> "${ago / 60} 分钟前"
            ago < 86400 -> "${ago / 3600} 小时前"
            else -> "${ago / 86400} 天前"
        }
        return "上次在线 $text"
    }

    fun formFactor(f: String): String = when (f) {
        "laptop" -> "笔记本"
        "mini" -> "迷你主机"
        "vm" -> "虚拟机"
        "server" -> "服务器"
        else -> "台式机"
    }

    fun os(os: String): String = when (os.lowercase()) {
        "macos" -> "macOS"
        "windows" -> "Windows"
        "linux" -> "Linux"
        else -> os.ifBlank { "未知系统" }
    }

    fun screens(n: Int): String = if (n <= 1) "单屏" else "$n 屏"

    fun summary(d: Device): String = buildList {
        add(os(d.os))
        d.resolution?.let { (w, h) -> add("$w\u00D7$h") }
        add(screens(d.screenCount))
    }.joinToString("\u2003")
}

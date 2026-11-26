// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch

import io.github.maxesisn.skylatch.hub.HubException
import io.github.maxesisn.skylatch.hub.HubJson
import io.github.maxesisn.skylatch.hub.HubText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class HubJsonTest {
    @Test
    fun devicesParseOnlineAndOfflineEntries() {
        val text = """
            {"ok":true,"devices":[
              {"device_id":"mac-mini","name":"Mac Mini","os":"macos","host_version":"0.2.1","monitors":1,
               "direct_endpoints":[{"kind":"lan","host":"10.0.0.3","port":7001}],"form_factor":"mini",
               "monitor_list":[{"id":"1","name":"DELL U2723QE","x":0,"y":0,"width":3840,"height":2160,"primary":true,"active":true}],
               "room":"Home","slot":3,"hardware_model":"Mac mini (Apple M4)","online":true},
              {"device_id":"vm-1","name":"","os":"linux","host_version":"0.1.0","monitors":2,"online":false,"last_seen_unix":1700000000,
               "room":null,"slot":null}
            ]}
        """.trimIndent()
        val (mini, vm) = HubJson.devices(text)
        assertEquals("mac-mini", mini.id)
        assertEquals("Mac mini (Apple M4)", mini.hardwareModel)
        assertEquals(3840 to 2160, mini.resolution)
        assertEquals(1, mini.screenCount)
        assertEquals(1, mini.directEndpoints)
        assertEquals(3L, mini.slot)
        assertTrue(mini.online)
        assertEquals("macOS\u20033840\u00D72160\u2003单屏", HubText.summary(mini))

        assertEquals("vm-1", vm.name)
        assertFalse(vm.online)
        assertEquals(1700000000L, vm.lastSeenUnix)
        assertNull(vm.room)
        assertNull(vm.slot)
        assertNull(vm.resolution)
        assertEquals(2, vm.screenCount)
        assertEquals("desktop", vm.formFactor)
    }

    @Test
    fun hubInfoLoginAndErrors() {
        val info = HubJson.hubInfo("""{"ok":true,"data":{"version":"0.2.1","mode":"relay","easytier_peers":["tcp://a:11010"],"allow_signup":true,"max_devices_per_account":20}}""")
        assertEquals("relay", info.mode)
        assertEquals(listOf("tcp://a:11010"), info.easytierPeers)
        assertTrue(info.allowSignup)
        assertEquals(20, info.maxDevicesPerAccount)
        assertNull(HubJson.hubInfo("""{"ok":true,"data":{"version":"1","mode":"signaling-only","easytier_peers":[],"allow_signup":false}}""").maxDevicesPerAccount)

        val login = HubJson.login("""{"ok":true,"token":"t-1","account":"max"}""")
        assertEquals("t-1", login.token)
        assertEquals("max", login.account)

        try {
            HubJson.devices("""{"ok":false,"error":"401 Unauthorized"}""")
            fail()
        } catch (e: HubException) {
            assertEquals("401 Unauthorized", e.message)
        }
        try {
            HubJson.ok("not json")
            fail()
        } catch (_: HubException) {
        }
        HubJson.ok("""{"ok":true}""")
    }

    @Test
    fun updateAndDownloadReplies() {
        val none = HubJson.update("""{"ok":true,"available":false,"version":"0.3.0"}""")
        assertFalse(none.available)
        assertNull(none.downloadUrl)
        val u = HubJson.update(
            """{"ok":true,"available":true,"version":"0.3.1","asset_name":"Skylatch-0.3.1-android-arm64.apk","size":7,
               "download_url":"https://x/a.apk","checksums_url":"https://x/SHA256SUMS","release_url":"https://x/r","notes":null}""",
        )
        assertTrue(u.available)
        assertEquals("Skylatch-0.3.1-android-arm64.apk", u.assetName)
        assertEquals(7L, u.size)
        assertNull(u.notes)
        val apk = HubJson.download("""{"ok":true,"path":"/c/a.apk","sha256":"ab","version":"0.3.1"}""")
        assertEquals("/c/a.apk", apk.path)
    }

    @Test
    fun monitorListFromTheSession() {
        val m = HubJson.monitors("""[{"id":"a","name":"Left","x":-1920,"y":0,"width":1920,"height":1080,"primary":false,"active":true}]""")
        assertEquals(-1920, m.single().x)
        assertTrue(m.single().active)
        assertEquals(emptyList<Any>(), HubJson.monitors("{bad"))
    }

    @Test
    fun hubTextMatchesTheDesktopClient() {
        assertEquals("wss://hub.example.com", HubText.normalizeHubUrl(" https://hub.example.com/ "))
        assertEquals("ws://10.0.0.2:17910", HubText.normalizeHubUrl("http://10.0.0.2:17910"))
        assertEquals("wss://hub", HubText.normalizeHubUrl("hub"))
        assertEquals("", HubText.normalizeHubUrl("  "))
        assertTrue(HubText.hubUrlValid("wss://hub"))
        assertFalse(HubText.hubUrlValid("wss://"))
        assertFalse(HubText.hubUrlValid("https://hub"))
        assertEquals("直连 + 中继", HubText.modeLabel("relay"))
        assertEquals("纯信令模式", HubText.modeLabel("signaling-only"))
        assertEquals("配套打洞节点：a，b", HubText.peersLine(listOf("a", "b")))
        assertEquals("上次在线 刚刚", HubText.lastSeen(100, 120))
        assertEquals("上次在线 12 分钟前", HubText.lastSeen(0, 12 * 60 + 5))
        assertEquals("上次在线 3 小时前", HubText.lastSeen(0, 3 * 3600))
        assertEquals("上次在线 2 天前", HubText.lastSeen(0, 2 * 86400))
        assertEquals("", HubText.lastSeen(null, 0))
    }
}

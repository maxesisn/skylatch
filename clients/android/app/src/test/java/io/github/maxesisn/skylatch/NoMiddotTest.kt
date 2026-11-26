// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

class NoMiddotTest {
    @Test
    fun noMiddotInAndroidUi() {
        val root = File("src/main")
        assertTrue("run from the app module: ${root.absolutePath}", root.isDirectory)
        val sources = root.walkTopDown().filter { it.isFile && it.extension in setOf("kt", "xml") }.toList()
        assertTrue(sources.size > 10)
        val hits = sources.flatMap { f ->
            f.readLines().mapIndexedNotNull { i, line ->
                if ('\u00B7' in line || "\\u00B7" in line || "\\u00b7" in line || "&#183;" in line || "&#xB7;" in line) "${f.path}:${i + 1}" else null
            }
        }
        assertEquals("middot found in: $hits", emptyList<String>(), hits)
    }
}

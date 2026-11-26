// SPDX-License-Identifier: Apache-2.0
package io.github.maxesisn.skylatch.stream

/** H.264 Annex B access units: NAL units behind `00 00 01` or `00 00 00 01` start codes. */
object AnnexB {
    const val NAL_SPS = 7
    const val NAL_PPS = 8

    /** One NAL unit: [type], its start code offset, and its payload `[start, end)`. */
    data class Nal(val type: Int, val codeStart: Int, val start: Int, val end: Int)

    fun nals(data: ByteArray): List<Nal> {
        val starts = ArrayList<IntArray>()
        var i = 0
        while (i + 2 < data.size) {
            if (data[i].toInt() == 0 && data[i + 1].toInt() == 0 && data[i + 2].toInt() == 1) {
                val codeStart = if (i > 0 && data[i - 1].toInt() == 0) i - 1 else i
                starts += intArrayOf(codeStart, i + 3)
                i += 3
            } else {
                i++
            }
        }
        return starts.mapIndexedNotNull { n, (codeStart, start) ->
            val end = if (n + 1 < starts.size) starts[n + 1][0] else data.size
            if (start < end) Nal(data[start].toInt() and 0x1f, codeStart, start, end) else null
        }
    }

    /** SPS and PPS of a keyframe, each with a 4-byte start code as `csd-0` / `csd-1` expect. */
    data class ParameterSets(val sps: ByteArray, val pps: ByteArray) {
        override fun equals(other: Any?): Boolean =
            other is ParameterSets && sps.contentEquals(other.sps) && pps.contentEquals(other.pps)

        override fun hashCode(): Int = 31 * sps.contentHashCode() + pps.contentHashCode()
    }

    fun parameterSets(data: ByteArray): ParameterSets? {
        val units = nals(data)
        val sps = units.firstOrNull { it.type == NAL_SPS } ?: return null
        val pps = units.firstOrNull { it.type == NAL_PPS } ?: return null
        return ParameterSets(withStartCode(data, sps), withStartCode(data, pps))
    }

    private fun withStartCode(data: ByteArray, nal: Nal): ByteArray {
        var end = nal.end
        while (end > nal.start && data[end - 1].toInt() == 0) end--
        val out = ByteArray(4 + end - nal.start)
        out[3] = 1
        System.arraycopy(data, nal.start, out, 4, end - nal.start)
        return out
    }
}

package app.magnetar.settings

/**
 * The few NIP-19 keys the app reads or writes, in plain Kotlin (the Rust core that did this
 * before is gone, D79): bech32 (BIP-173) decode/encode, `npub` and `nprofile` → hex pubkey,
 * hex → `npub`. Private keys are never decoded: [isPrivateKey] stops them before this is called.
 */
object Nip19 {
    private const val CHARSET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"
    private val GEN = intArrayOf(0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3)
    private val HEX64 = Regex("^[0-9a-f]{64}$")

    /** NIP-19 strings can be longer than BIP-173's 90 characters (nprofile with relays). */
    private const val MAX_LEN = 5000

    private fun polymod(values: IntArray): Int {
        var chk = 1
        for (v in values) {
            val b = chk ushr 25
            chk = ((chk and 0x1ffffff) shl 5) xor v
            for (i in 0 until 5) if ((b ushr i) and 1 == 1) chk = chk xor GEN[i]
        }
        return chk
    }

    private fun hrpExpand(hrp: String): IntArray =
        IntArray(hrp.length * 2 + 1).also { out ->
            hrp.forEachIndexed { i, c ->
                out[i] = c.code ushr 5
                out[i + hrp.length + 1] = c.code and 31
            }
        }

    /** (hrp, 5-bit data without checksum), or null for anything that is not valid bech32. */
    fun decodeBech32(s: String): Pair<String, IntArray>? {
        if (s.length < 8 || s.length > MAX_LEN) return null
        if (s.any { it.code < 33 || it.code > 126 }) return null
        if (s != s.lowercase() && s != s.uppercase()) return null
        val str = s.lowercase()
        val sep = str.lastIndexOf('1')
        if (sep < 1 || sep + 7 > str.length) return null
        val hrp = str.substring(0, sep)
        val data = IntArray(str.length - sep - 1)
        for (i in data.indices) {
            val v = CHARSET.indexOf(str[sep + 1 + i])
            if (v < 0) return null
            data[i] = v
        }
        if (polymod(hrpExpand(hrp) + data) != 1) return null
        return hrp to data.copyOf(data.size - 6)
    }

    fun encodeBech32(hrp: String, data5: IntArray): String {
        val values = hrpExpand(hrp) + data5 + IntArray(6)
        val mod = polymod(values) xor 1
        val sb = StringBuilder(hrp).append('1')
        for (v in data5) sb.append(CHARSET[v])
        for (i in 0 until 6) sb.append(CHARSET[(mod ushr (5 * (5 - i))) and 31])
        return sb.toString()
    }

    /** Regroups bits (8 → 5 to encode, 5 → 8 to decode); null on invalid padding. */
    private fun convertBits(data: IntArray, from: Int, to: Int, pad: Boolean): IntArray? {
        var acc = 0
        var bits = 0
        val out = ArrayList<Int>()
        val maxv = (1 shl to) - 1
        for (v in data) {
            if (v < 0 || v ushr from != 0) return null
            acc = (acc shl from) or v
            bits += from
            while (bits >= to) {
                bits -= to
                out += (acc ushr bits) and maxv
            }
            acc = acc and ((1 shl bits) - 1)
        }
        if (pad) {
            if (bits > 0) out += (acc shl (to - bits)) and maxv
        } else if (bits >= from || ((acc shl (to - bits)) and maxv) != 0) {
            return null
        }
        return out.toIntArray()
    }

    private fun hex(bytes: IntArray): String = bytes.joinToString("") { "%02x".format(it) }

    /**
     * The hex public key of an `npub1…`, an `nprofile1…` (TLV type 0) or a 64-digit hex key;
     * null for anything else (an nsec is never decoded).
     */
    fun pubkeyHex(input: String): String? {
        val s = input.trim()
        if (isPrivateKey(s)) return null
        if (HEX64.matches(s.lowercase())) return s.lowercase()
        val (hrp, data5) = decodeBech32(s) ?: return null
        val bytes = convertBits(data5, 5, 8, pad = false) ?: return null
        return when (hrp) {
            "npub" -> bytes.takeIf { it.size == 32 }?.let(::hex)
            "nprofile" -> {
                var i = 0
                while (i + 2 <= bytes.size) {
                    val t = bytes[i]
                    val l = bytes[i + 1]
                    if (i + 2 + l > bytes.size) return null
                    if (t == 0 && l == 32) return hex(bytes.copyOfRange(i + 2, i + 34))
                    i += 2 + l
                }
                null
            }
            else -> null
        }
    }

    /** `npub1…` of a 64-digit hex pubkey; [hex] itself when it is not one. */
    fun npub(hex: String): String {
        val h = hex.trim().lowercase()
        if (!HEX64.matches(h)) return hex
        val bytes = IntArray(32) { h.substring(it * 2, it * 2 + 2).toInt(16) }
        return encodeBech32("npub", convertBits(bytes, 8, 5, pad = true)!!)
    }
}

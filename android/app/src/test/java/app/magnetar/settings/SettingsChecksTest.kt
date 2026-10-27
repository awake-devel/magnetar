package app.magnetar.settings

import org.testng.Assert.assertEquals
import org.testng.Assert.assertFalse
import org.testng.Assert.assertNotNull
import org.testng.Assert.assertNull
import org.testng.Assert.assertTrue
import org.testng.annotations.Test

class SettingsChecksTest {
    // NIP-19's own example (nips/19.md).
    private val hex = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"
    private val npub = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6"
    private val nprofile =
        "nprofile1qqsrhuxx8l9ex335q7he0f09aej04zpazpl0ne2cgukyawd24mayt8gpp4mhxue69uhhytnc9e3k7mgpz4mhxue69uhkg6nzv9ejuumpv34kytnrdaksjlyr9p"

    @Test
    fun `npub and nprofile decode to the NIP-19 example key, and back`() {
        assertEquals(Nip19.pubkeyHex(npub), hex)
        assertEquals(Nip19.pubkeyHex(" $npub "), hex)
        assertEquals(Nip19.pubkeyHex(npub.uppercase()), hex)
        assertEquals(Nip19.pubkeyHex(nprofile), hex)
        assertEquals(Nip19.pubkeyHex(hex.uppercase()), hex)
        assertEquals(Nip19.npub(hex), npub)
        assertEquals(Nip19.npub("not hex"), "not hex")
    }

    @Test
    fun `bad keys are refused`() {
        // One character changed: the checksum fails.
        assertNull(Nip19.pubkeyHex(npub.dropLast(1) + "7"))
        assertNull(Nip19.pubkeyHex("npub1"))
        assertNull(Nip19.pubkeyHex(npub.substring(0, 20) + npub.substring(20).uppercase()))
        assertNull(Nip19.pubkeyHex(hex.dropLast(2)))
        // A valid bech32 string of another kind (note1…) is not a public key.
        val note = Nip19.encodeBech32("note", Nip19.decodeBech32(npub)!!.second)
        assertNull(Nip19.pubkeyHex(note))
        // Private keys are never decoded.
        val nsec = Nip19.encodeBech32("nsec", Nip19.decodeBech32(npub)!!.second)
        assertNull(Nip19.pubkeyHex(nsec))
    }

    @Test
    fun `bech32 matches BIP-173 test vectors`() {
        for (v in listOf("A12UEL5L", "a12uel5l", "abcdef1qpzry9x8gf2tvdw0s3jn54khce6mua7lmqqqxw", "split1checkupstagehandshakeupstreamerranterredcaperred2y9e3w")) {
            assertNotNull(Nip19.decodeBech32(v), v)
        }
        for (v in listOf("pzry9x0s0muk", "1pzry9x0s0muk", "x1b4n0q5v", "li1dgmt3", "A1G7SGD8", "10a06t8")) {
            assertNull(Nip19.decodeBech32(v), v)
        }
        val (hrp, data) = Nip19.decodeBech32("abcdef1qpzry9x8gf2tvdw0s3jn54khce6mua7lmqqqxw")!!
        assertEquals(Nip19.encodeBech32(hrp, data), "abcdef1qpzry9x8gf2tvdw0s3jn54khce6mua7lmqqqxw")
    }

    @Test
    fun `root key field`() {
        assertNull(checkRootKey("").error)
        assertNull(checkRootKey(npub).error)
        assertNull(checkRootKey(hex).error)
        assertEquals(checkRootKey("nsec1abc").error, PRIVATE_KEY_ERROR)
        assertEquals(checkRootKey("NCRYPTSEC1abc").error, PRIVATE_KEY_ERROR)
        assertNotNull(checkRootKey("hello").error)
    }

    @Test
    fun `server URL field`() {
        assertEquals(checkServerUrl(""), Check())
        assertEquals(checkServerUrl("https://ryoku.tail26814a.ts.net"), Check())
        assertEquals(checkServerUrl("http://magnetar.tail1234.ts.net:8787/"), Check())
        assertEquals(checkServerUrl("http://localhost:8787"), Check())
        assertEquals(checkServerUrl("http://127.0.0.1:8787"), Check())
        assertEquals(checkServerUrl("http://[fd7a:115c:a1e0::1]:8787"), Check())
        assertNotNull(checkServerUrl("http://192.168.1.20:8787").warning)
        assertTrue(checkServerUrl("http://100.81.1.2:8787").warning!!.contains("100.x"))
        assertNull(checkServerUrl("http://100.81.1.2:8787").error)
        assertEquals(checkServerUrl("ryoku.ts.net").error, "Start with https:// or http://")
        assertEquals(checkServerUrl("ftp://host").error, "Must be an http:// or https:// URL")
        assertNotNull(checkServerUrl("https://").error)
        assertFalse(isPrivateTransport("100.81.1.2"))
        assertTrue(isTailscaleCgnat("100.64.0.1"))
        assertFalse(isTailscaleCgnat("100.128.0.1"))
    }

    @Test
    fun `token field`() {
        assertNull(checkToken("").error)
        assertNull(checkToken("  abc123-XYZ_.~  ").error)
        assertNotNull(checkToken("abc\ndef").error)
        assertNotNull(checkToken("abc def").error)
        assertNotNull(checkToken("tökén").error)
    }
}

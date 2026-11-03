// Port of android/.../settings/Nip19.kt: bech32 (BIP-173) decode and encode, npub and nprofile →
// hex pubkey, hex → npub. Private keys are never decoded: isPrivateKey stops them first.

const CHARSET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const GEN = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
const HEX64 = /^[0-9a-f]{64}$/;
/** NIP-19 strings can be longer than BIP-173's 90 characters (nprofile with relays). */
const MAX_LEN = 5000;

/**
 * True for a Nostr private key (bech32 nsec or ncryptsec), which the trust root must never hold.
 * @param {string} key
 */
export function isPrivateKey(key) {
  const k = key.trim().toLowerCase();
  return k.startsWith("nsec1") || k.startsWith("ncryptsec1");
}

/** @param {number[]} values */
function polymod(values) {
  let chk = 1;
  for (const v of values) {
    const b = chk >>> 25;
    chk = (((chk & 0x1ffffff) << 5) ^ v) >>> 0;
    for (let i = 0; i < 5; i++) if (((b >>> i) & 1) === 1) chk = (chk ^ GEN[i]) >>> 0;
  }
  return chk;
}

/** @param {string} hrp */
function hrpExpand(hrp) {
  const out = new Array(hrp.length * 2 + 1).fill(0);
  for (let i = 0; i < hrp.length; i++) {
    const c = hrp.charCodeAt(i);
    out[i] = c >>> 5;
    out[i + hrp.length + 1] = c & 31;
  }
  return out;
}

/**
 * (hrp, 5-bit data without checksum), or null for anything that is not valid bech32.
 * @param {string} s
 * @returns {[string, number[]] | null}
 */
export function decodeBech32(s) {
  if (s.length < 8 || s.length > MAX_LEN) return null;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 33 || c > 126) return null;
  }
  if (s !== s.toLowerCase() && s !== s.toUpperCase()) return null;
  const str = s.toLowerCase();
  const sep = str.lastIndexOf("1");
  if (sep < 1 || sep + 7 > str.length) return null;
  const hrp = str.slice(0, sep);
  /** @type {number[]} */
  const data = [];
  for (let i = sep + 1; i < str.length; i++) {
    const v = CHARSET.indexOf(str[i]);
    if (v < 0) return null;
    data.push(v);
  }
  if (polymod([...hrpExpand(hrp), ...data]) !== 1) return null;
  return [hrp, data.slice(0, data.length - 6)];
}

/**
 * @param {string} hrp
 * @param {number[]} data5
 */
export function encodeBech32(hrp, data5) {
  const values = [...hrpExpand(hrp), ...data5, 0, 0, 0, 0, 0, 0];
  const mod = (polymod(values) ^ 1) >>> 0;
  let s = hrp + "1";
  for (const v of data5) s += CHARSET[v];
  for (let i = 0; i < 6; i++) s += CHARSET[(mod >>> (5 * (5 - i))) & 31];
  return s;
}

/**
 * Regroups bits (8 → 5 to encode, 5 → 8 to decode); null on invalid padding.
 * @param {number[]} data
 * @param {number} from
 * @param {number} to
 * @param {boolean} pad
 * @returns {number[] | null}
 */
function convertBits(data, from, to, pad) {
  let acc = 0;
  let bits = 0;
  /** @type {number[]} */
  const out = [];
  const maxv = (1 << to) - 1;
  for (const v of data) {
    if (v < 0 || v >>> from !== 0) return null;
    acc = (acc << from) | v;
    bits += from;
    while (bits >= to) {
      bits -= to;
      out.push((acc >>> bits) & maxv);
    }
    acc &= (1 << bits) - 1;
  }
  if (pad) {
    if (bits > 0) out.push((acc << (to - bits)) & maxv);
  } else if (bits >= from || ((acc << (to - bits)) & maxv) !== 0) {
    return null;
  }
  return out;
}

/** @param {number[]} bytes */
function hex(bytes) {
  return bytes.map((b) => b.toString(16).padStart(2, "0")).join("");
}

/**
 * The hex public key of an npub1…, an nprofile1… (TLV type 0) or a 64-digit hex key; null for
 * anything else (an nsec is never decoded).
 * @param {string} input
 * @returns {string | null}
 */
export function pubkeyHex(input) {
  const s = input.trim();
  if (isPrivateKey(s)) return null;
  if (HEX64.test(s.toLowerCase())) return s.toLowerCase();
  const dec = decodeBech32(s);
  if (dec == null) return null;
  const [hrp, data5] = dec;
  const bytes = convertBits(data5, 5, 8, false);
  if (bytes == null) return null;
  if (hrp === "npub") return bytes.length === 32 ? hex(bytes) : null;
  if (hrp === "nprofile") {
    let i = 0;
    while (i + 2 <= bytes.length) {
      const t = bytes[i];
      const l = bytes[i + 1];
      if (i + 2 + l > bytes.length) return null;
      if (t === 0 && l === 32) return hex(bytes.slice(i + 2, i + 34));
      i += 2 + l;
    }
    return null;
  }
  return null;
}

/**
 * npub1… of a 64-digit hex pubkey; the input itself when it is not one.
 * @param {string} hexKey
 */
export function npub(hexKey) {
  const h = hexKey.trim().toLowerCase();
  if (!HEX64.test(h)) return hexKey;
  /** @type {number[]} */
  const bytes = [];
  for (let i = 0; i < 32; i++) bytes.push(parseInt(h.slice(i * 2, i * 2 + 2), 16));
  return encodeBech32("npub", /** @type {number[]} */ (convertBits(bytes, 8, 5, true)));
}

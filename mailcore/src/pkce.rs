//! PKCE (RFC 7636) helpers, plus the small crypto/URL primitives `auth`
//! needs to build them: a hand-rolled SHA-256 (FIPS 180-4), a base64url
//! (no padding) codec, and an `application/x-www-form-urlencoded` encoder.
//! No crypto crate — SHA-256 and OS randomness are the only primitives
//! auth-code + PKCE requires, and pulling in a dependency for them would
//! break the "hand-roll it" spirit of the rest of this crate.

/// A PKCE verifier/challenge pair (RFC 7636, S256 method).
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// Generates a fresh code verifier (32 bytes of OS randomness,
    /// base64url-encoded) and its S256 code challenge.
    pub fn generate() -> Pkce {
        let verifier = base64url_encode(&random_bytes(32));
        let challenge = base64url_encode(&sha256(verifier.as_bytes()));
        Pkce {
            verifier,
            challenge,
        }
    }
}

/// Reads `n` bytes of OS-provided randomness.
///
/// `pub(crate)` (rather than private) so `store` can reuse this same
/// OS-entropy source for the `local:<hex>` draft id, instead of pulling in
/// a `uuid` crate for something this small.
#[cfg(windows)]
pub(crate) fn random_bytes(n: usize) -> Vec<u8> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };

    let mut buf = vec![0u8; n];
    // SAFETY: `buf` is a valid, uniquely-owned buffer of length `n`; we pass
    // its exact length and a null algorithm handle, which per the BCrypt
    // docs selects the system-preferred RNG (no handle to leak/close).
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    assert!(status == 0, "BCryptGenRandom failed: NTSTATUS {status:#x}");
    buf
}

/// Reads `n` bytes of OS-provided randomness. See the `windows` cfg of this
/// same function for why this is `pub(crate)`.
#[cfg(not(windows))]
pub(crate) fn random_bytes(n: usize) -> Vec<u8> {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    let mut f = std::fs::File::open("/dev/urandom").expect("open /dev/urandom");
    f.read_exact(&mut buf).expect("read /dev/urandom");
    buf
}

/// SHA-256 (FIPS 180-4), hand-rolled so no crypto crate enters the
/// dependency tree. Used only for the PKCE S256 code challenge, a value
/// sent openly over the wire — not a secret-handling path where
/// side-channel resistance would matter.
pub fn sha256(input: &[u8]) -> [u8; 32] {
    #[rustfmt::skip]
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // Padding: a single 1-bit (0x80 byte), zeros, then the 64-bit
    // big-endian bit length, so the total length is a multiple of 64 bytes.
    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut msg = input.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64url (RFC 4648 section 5), no padding.
pub fn base64url_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3f) as usize] as char);
        }
    }
    out
}

/// Decodes base64url (RFC 4648 section 5, no padding). Returns `None` on
/// invalid characters or a length that can't be a valid unpadded encoding
/// (one leftover character, which is fewer than 8 bits of data).
pub fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    fn digit(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        })
    }

    let bytes = s.as_bytes();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = [0u32; 4];
        for (i, &c) in chunk.iter().enumerate() {
            n[i] = digit(c)?;
        }
        let combined = (n[0] << 18) | (n[1] << 12) | (n[2] << 6) | n[3];
        out.push((combined >> 16) as u8);
        if chunk.len() > 2 {
            out.push((combined >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(combined as u8);
        }
    }
    Some(out)
}

/// Percent-encodes `pairs` as `application/x-www-form-urlencoded`
/// (`key=value&key2=value2`); also used to build query strings. Unreserved
/// characters (`A-Za-z0-9-_.~`) pass through; everything else is
/// percent-encoded.
pub fn form_urlencode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod pkce_tests {
    use super::*;
    #[test]
    fn sha256_known_vector() {
        // sha256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let d = sha256(b"abc");
        assert_eq!(
            base64url_encode(&d),
            "ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0"
        );
    }
    #[test]
    fn challenge_is_base64url_of_sha256_of_verifier() {
        let p = Pkce::generate();
        assert_eq!(
            p.challenge,
            base64url_encode(&sha256(p.verifier.as_bytes()))
        );
        assert!(
            !p.verifier.contains('=') && !p.verifier.contains('+') && !p.verifier.contains('/')
        );
    }
    #[test]
    fn base64url_roundtrips() {
        let b = [0u8, 1, 2, 250, 251, 252, 253];
        assert_eq!(base64url_decode(&base64url_encode(&b)).unwrap(), b);
    }
}

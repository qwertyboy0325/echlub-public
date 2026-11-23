//! MD5 (RFC 1321), written out in full: FLAC's STREAMINFO carries the MD5
//! of the unencoded audio. Streaming (`update` any number of times, then
//! `finish`). Not for security: only to check that audio survived a codec.

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10,
    15, 21, 6, 10, 15, 21,
];

/// `floor(abs(sin(i + 1)) * 2^32)`, the RFC's table, built the same way
/// (checked against the RFC's first and last entries by the tests).
fn k_table() -> [u32; 64] {
    let mut k = [0u32; 64];
    for (i, v) in k.iter_mut().enumerate() {
        *v = (((i as f64) + 1.0).sin().abs() * 4_294_967_296.0) as u32;
    }
    k
}

#[derive(Clone)]
pub struct Md5 {
    state: [u32; 4],
    k: [u32; 64],
    buf: [u8; 64],
    filled: usize,
    len: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Md5 {
    pub fn new() -> Md5 {
        Md5 { state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476], k: k_table(), buf: [0; 64], filled: 0, len: 0 }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);
        if self.filled > 0 {
            let take = (64 - self.filled).min(data.len());
            self.buf[self.filled..self.filled + take].copy_from_slice(&data[..take]);
            self.filled += take;
            data = &data[take..];
            if self.filled < 64 {
                return;
            }
            let block = self.buf;
            self.block(&block);
            self.filled = 0;
        }
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            self.block(&block);
            data = &data[64..];
        }
        self.buf[..data.len()].copy_from_slice(data);
        self.filled = data.len();
    }

    pub fn finish(mut self) -> [u8; 16] {
        let bits = self.len.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        while (self.len as usize + pad.len()) % 64 != 56 {
            pad.push(0);
        }
        pad.extend_from_slice(&bits.to_le_bytes());
        self.update(&pad);
        debug_assert_eq!(self.filled, 0);
        let mut out = [0u8; 16];
        for (i, w) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        out
    }

    fn block(&mut self, block: &[u8; 64]) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        for (i, (&k, &rot)) in self.k.iter().zip(S.iter()).enumerate() {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(rot));
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }
}

/// The MD5 of `data`.
pub fn md5(data: &[u8]) -> [u8; 16] {
    let mut m = Md5::new();
    m.update(data);
    m.finish()
}

/// 32 lowercase hex digits.
pub fn hex(d: &[u8; 16]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 1321's test suite (appendix A.5).
    #[test]
    fn the_rfc_1321_vectors() {
        let cases: [(&str, &str); 7] = [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            ("abcdefghijklmnopqrstuvwxyz", "c3fcd3d76192e4007dfb496cca67e13b"),
            ("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789", "d174ab98d277d9f5a5611c2c9f419d9f"),
            ("12345678901234567890123456789012345678901234567890123456789012345678901234567890", "57edf4a22be3c955ac49da2e2107b67a"),
        ];
        for (text, want) in cases {
            assert_eq!(hex(&md5(text.as_bytes())), want, "{text:?}");
        }
    }

    /// One million `a`, fed in pieces of every awkward size.
    #[test]
    fn a_million_a_in_any_chunking() {
        let data = vec![b'a'; 1_000_000];
        assert_eq!(hex(&md5(&data)), "7707d6ae4e027c70eea2a935c2296f21");
        for chunk in [1usize, 3, 55, 56, 63, 64, 65, 1000, 4096] {
            let mut m = Md5::new();
            for c in data.chunks(chunk) {
                m.update(c);
            }
            assert_eq!(hex(&m.finish()), "7707d6ae4e027c70eea2a935c2296f21", "chunks of {chunk}");
        }
    }

    /// Lengths around the padding boundary (55, 56, 63, 64 bytes) against
    /// values from the system's `md5` tool (`printf 'a%.0s' ... | md5`).
    #[test]
    fn padding_boundaries() {
        let want = [
            (55, "ef1772b6dff9a122358552954ad0df65"),
            (56, "3b0c8ac703f828b04c6c197006d17218"),
            (63, "b06521f39153d618550606be297466d5"),
            (64, "014842d480b571495a4a0363793f7367"),
        ];
        for (n, h) in want {
            assert_eq!(hex(&md5(&vec![b'a'; n])), h, "{n} bytes");
        }
    }

    #[test]
    fn the_sine_table_matches_the_rfc() {
        let k = k_table();
        assert_eq!((k[0], k[1], k[63]), (0xd76a_a478, 0xe8c7_b756, 0xeb86_d391));
    }
}

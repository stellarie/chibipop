//! A PNG writer for the capture dump hook, and nothing else.
//!
//! Stored (uncompressed) deflate blocks inside the zlib stream an IDAT
//! chunk requires: a file every decoder reads, in one screen of code
//! and no new dependency. Dump files are throwaway diagnostics written
//! by hand at a human's pace, so their bytes are cheaper than a
//! compression crate in the daemon's dependency tree.

/// Bytes of a PNG holding `bgra`, which is `w * h * 4` in core's
/// `Frame` layout. Alpha is dropped: it is junk by contract.
pub fn encode(bgra: &[u8], w: u32, h: u32) -> Vec<u8> {
    let raw = scanlines(bgra, w, h);
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 8192 * 5 + 64);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    // 8-bit, colour type 2 (truecolour), deflate, no filter, no interlace.
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Filter byte plus RGB triples, per row.
fn scanlines(bgra: &[u8], w: u32, h: u32) -> Vec<u8> {
    let row = w as usize * 3 + 1;
    let mut raw = vec![0u8; row * h as usize];
    for y in 0..h as usize {
        let src = y * w as usize * 4;
        let dst = y * row + 1;
        for x in 0..w as usize {
            let (s, d) = (src + x * 4, dst + x * 3);
            let Some(px) = bgra.get(s..s + 4) else { return raw };
            raw[d] = px[2];
            raw[d + 1] = px[1];
            raw[d + 2] = px[0];
        }
    }
    raw
}

/// A zlib stream of stored deflate blocks.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    // CM=8, CINFO=7, FLEVEL=0, FCHECK making the header word a multiple
    // of 31: the fixed 0x78 0x01 pair.
    let mut z = vec![0x78, 0x01];
    let mut rest = raw;
    loop {
        let take = rest.len().min(0xffff);
        let (block, tail) = rest.split_at(take);
        let last = u8::from(tail.is_empty());
        z.push(last);
        z.extend_from_slice(&(take as u16).to_le_bytes());
        z.extend_from_slice(&(!(take as u16)).to_le_bytes());
        z.extend_from_slice(block);
        if tail.is_empty() {
            break;
        }
        rest = tail;
    }
    z.extend_from_slice(&adler32(raw).to_be_bytes());
    z
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(data, crc32(kind, !0));
    out.extend_from_slice(&(!crc).to_be_bytes());
}

/// Streaming CRC-32: `state` chains a chunk's type and data without
/// joining them into one buffer.
fn crc32(bytes: &[u8], state: u32) -> u32 {
    let mut crc = state;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in bytes {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk the chunk list, checking every length and CRC.
    fn chunks(png: &[u8]) -> Vec<(String, Vec<u8>)> {
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        let mut at = 8;
        let mut out = Vec::new();
        while at < png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let kind = String::from_utf8(png[at + 4..at + 8].to_vec()).unwrap();
            let data = png[at + 8..at + 8 + len].to_vec();
            let want = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
            let got = !crc32(&data, crc32(&png[at + 4..at + 8], !0));
            assert_eq!(got, want, "{kind} CRC");
            out.push((kind, data));
            at += 12 + len;
        }
        out
    }

    /// Inflate a stream of stored blocks and check the adler.
    fn inflate_stored(z: &[u8]) -> Vec<u8> {
        assert_eq!(&z[..2], &[0x78, 0x01]);
        let mut at = 2;
        let mut out = Vec::new();
        loop {
            let last = z[at];
            let len = u16::from_le_bytes(z[at + 1..at + 3].try_into().unwrap()) as usize;
            let nlen = u16::from_le_bytes(z[at + 3..at + 5].try_into().unwrap());
            assert_eq!(nlen, !(len as u16), "NLEN must be LEN's complement");
            out.extend_from_slice(&z[at + 5..at + 5 + len]);
            at += 5 + len;
            if last == 1 {
                break;
            }
        }
        let want = u32::from_be_bytes(z[at..at + 4].try_into().unwrap());
        assert_eq!(adler32(&out), want, "adler32");
        assert_eq!(at + 4, z.len(), "trailing bytes after the stream");
        out
    }

    #[test]
    fn one_red_pixel_round_trips() {
        // BGRA: blue 0, green 0, red 255.
        let png = encode(&[0, 0, 0xff, 0xff], 1, 1);
        let parts = chunks(&png);
        let kinds: Vec<&str> = parts.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
        assert_eq!(parts[0].1, vec![0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
        // Filter byte then R, G, B.
        assert_eq!(inflate_stored(&parts[1].1), vec![0, 0xff, 0, 0]);
    }

    #[test]
    fn rows_carry_their_own_filter_byte() {
        let bgra = vec![
            1, 2, 3, 0xff, 4, 5, 6, 0xff, // row 0
            7, 8, 9, 0xff, 10, 11, 12, 0xff, // row 1
        ];
        let png = encode(&bgra, 2, 2);
        let raw = inflate_stored(&chunks(&png)[1].1);
        assert_eq!(raw, vec![0, 3, 2, 1, 6, 5, 4, 0, 9, 8, 7, 12, 11, 10]);
    }

    /// A picture bigger than one stored block must still decode.
    #[test]
    fn a_large_image_spans_several_blocks() {
        let (w, h) = (200u32, 120u32);
        let bgra: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let png = encode(&bgra, w, h);
        let raw = inflate_stored(&chunks(&png)[1].1);
        assert_eq!(raw.len(), (w as usize * 3 + 1) * h as usize);
        assert!(raw.len() > 0xffff, "the test image must exceed one block");
    }

    #[test]
    fn a_short_buffer_does_not_panic() {
        let png = encode(&[0, 0, 0, 0], 4, 4);
        assert!(!png.is_empty());
    }
}

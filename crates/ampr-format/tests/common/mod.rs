//! Helpers the integration tests share.

/// `index` with the chunks of each volume laid out backwards: the volume's
/// last chunk id at byte 64, then the one before it, back to back, so every
/// volume is still exactly full. The payload CRC is sealed again.
pub fn backwards(index: &[u8]) -> Vec<u8> {
    let mut out = index.to_vec();
    let u64_at = |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
    let (count, chunks_at) = (u64_at(&out, 48) as usize, u64_at(&out, 64) as usize);
    let volumes = u32::from_le_bytes(out[20..24].try_into().unwrap()) as usize;
    let mut ends = vec![64u64; volumes];
    for id in (0..count).rev() {
        let at = chunks_at + 8 * id;
        let c = u64_at(&out, at);
        let v = ((c >> 32) & 0xfff) as usize;
        let moved = (c & !0xffff_ffff) | ends[v];
        ends[v] += ((c >> 44) & 0x3ffff) + 1;
        out[at..at + 8].copy_from_slice(&moved.to_le_bytes());
    }
    let crc = ampr_format::crc32(&out[128..]);
    out[112..116].copy_from_slice(&crc.to_le_bytes());
    out
}

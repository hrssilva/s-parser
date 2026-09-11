/// Skip verified ordinary ASCII IRI bytes in full vectors. Delimiters, escapes,
/// forbidden bytes, non-ASCII bytes and short tails stay with the scalar scanner.
/// Returning zero is always valid: this is an optimization, not a second grammar.
#[inline]
pub(super) fn ascii_iri_prefix(bytes: &[u8]) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::*;

        let mut position = 0;
        // SAFETY: SSE2 is part of the x86_64 baseline. The length check proves
        // each unaligned 16-byte load is entirely inside `bytes`; no padding,
        // pointer alignment, or readable bytes past the slice are required.
        unsafe {
            while bytes.len() - position >= 16 {
                let lanes = _mm_loadu_si128(bytes.as_ptr().add(position).cast());
                // The signed comparison marks controls/spaces AND non-ASCII
                // bytes, handing Unicode back to the scalar/UTF-8 validators.
                let mut special = _mm_cmpgt_epi8(_mm_set1_epi8(33), lanes);
                for byte in *b"\"<>\\^`{|}" {
                    special =
                        _mm_or_si128(special, _mm_cmpeq_epi8(lanes, _mm_set1_epi8(byte as i8)));
                }
                let mask = _mm_movemask_epi8(special) as u32;
                if mask != 0 {
                    return position + mask.trailing_zeros() as usize;
                }
                position += 16;
            }
        }
        position
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = bytes;
        0
    }
}

use super::*;

fn agrees_with_scalar(bytes: &[u8], eof: bool) {
    let scalar = decode::<false>(bytes, eof).map(|d| (d.tokens, d.consumed));
    let accelerated = NtTokenizer
        .decode(bytes, eof)
        .map(|d| (d.tokens, d.consumed));
    assert_eq!(scalar, accelerated, "eof={eof}, bytes={bytes:?}");
}

#[test]
fn simd_matches_scalar_on_byte_mutations_and_incomplete_records() {
    let input = b"<https://example.org/abcdefghijklmnopqrstuvwxyz/abcdefghijklmnopqrstuvwxyz/abcdefghijklmnopqrstuvwxyz> <urn:predicate> <https://example.org/abcdefghijklmnopqrstuvwxyz/abcdefghijklmnopqrstuvwxyz> .\n";
    let mut cases = 0;
    for position in 0..input.len() {
        let mut bytes = input.to_vec();
        for value in 0..=255u8 {
            bytes[position] = value;
            agrees_with_scalar(&bytes, true);
            agrees_with_scalar(&bytes, false);
            cases += 2;
        }
    }
    for data in [
        input.as_slice(),
        "<https://example.org/abcdefghijklmnop/é/🦀/\\u0041/abcdefghijklmnop> <urn:p> <urn:o>.\n"
            .as_bytes(),
    ] {
        for end in 0..=data.len() {
            agrees_with_scalar(&data[..end], true);
            agrees_with_scalar(&data[..end], false);
            cases += 2;
        }
    }
    assert_eq!(cases, 101_438);
}

#[test]
fn mixed_unicode_escapes_and_short_iris_match_scalar_at_every_alignment() {
    for content in [
        "",
        "x",
        "abcdefghijklmnop",
        "abcdefghijklmnopqrstuvwxyz0123456789",
        "é",
        "🦀",
        "abcédef🦀ghi",
        "\\u0041",
        "\\U0001F980",
        "\\u003E",
        "\\uD800",
        "\\U00110000",
        "\\q",
        "with space",
        "with\nnewline",
    ] {
        for padding in 0..32 {
            for term in [
                format!("<urn:{}{content}>", "a".repeat(padding)),
                format!("<\\u0075rn:{}{content}>", "a".repeat(padding)),
            ] {
                for line in [
                    format!("{term}<urn:p><urn:o>.\n"),
                    format!("<urn:s>{term}<urn:o>.\r\n"),
                    format!("<urn:s><urn:p>{term}."),
                    format!("<urn:s><urn:p>\"value\"^^{term}.\n"),
                ] {
                    agrees_with_scalar(line.as_bytes(), true);
                    agrees_with_scalar(line.as_bytes(), false);
                }
            }
        }
    }
}

#[test]
fn bounded_windows_and_partitions_match_scalar_records() {
    use crate::{QuadReader, SliceStream};

    struct Scalar;
    impl Tokenizer for Scalar {
        type Output = [Token; 4];
        fn decode(&mut self, bytes: &[u8], eof: bool) -> Result<Decoded> {
            decode::<false>(bytes, eof)
        }
    }
    let line = "<https://example.org/abcdefghijklmnop/é/\\u0041> <https://example.org/abcdefghijklmnop/predicate> \"🦀\"^^<https://example.org/abcdefghijklmnop/datatype>.\r\n";
    let input = format!("# header\n{}\n{}", line.repeat(3), line.trim_end());
    let data = input.as_bytes();
    let mut scalar = QuadReader::new(SliceStream::new(data, data.len()).unwrap(), Scalar);
    let mut expected = Vec::new();
    while let Some(q) = scalar.next().unwrap() {
        expected.push((q.offset(), *q.tokens(), q.bytes().to_vec()));
    }
    for window in line.len()..=data.len() {
        let mut reader = QuadReader::new(SliceStream::new(data, window).unwrap(), NtTokenizer);
        let mut actual = Vec::new();
        while let Some(q) = reader.next().unwrap() {
            actual.push((q.offset(), *q.tokens(), q.bytes().to_vec()));
        }
        assert_eq!(actual, expected, "window={window}");
    }
    for split in 0..=data.len() as u64 {
        let mut actual = Vec::new();
        for range in [0..split, split..data.len() as u64] {
            let mut reader = QuadReader::for_range(
                SliceStream::new(data, line.len()).unwrap(),
                NtTokenizer,
                range,
            )
            .unwrap();
            while let Some(q) = reader.next().unwrap() {
                actual.push((q.offset(), *q.tokens(), q.bytes().to_vec()));
            }
        }
        assert_eq!(actual, expected, "split={split}");
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn vector_classification_covers_every_byte_lane_and_alignment() {
    for alignment in 0..16 {
        let mut storage = [b'a'; 80];
        let bytes = &mut storage[alignment..alignment + 64];
        for position in 0..64 {
            for byte in 0..=255u8 {
                bytes[position] = byte;
                let ordinary = byte.is_ascii() && byte > b' ' && !b"\"<>\\^`{|}".contains(&byte);
                let expected = if ordinary { bytes.len() } else { position };
                assert_eq!(
                    simd::ascii_iri_prefix(bytes),
                    expected,
                    "alignment={alignment}, position={position}, byte={byte}"
                );
            }
            bytes[position] = b'a';
        }
    }
}

// A vector overread must fault here, even if all normal test allocations happen
// to have readable padding. No Rust reference ever includes a protected page.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn vector_loads_stay_inside_slices_next_to_guard_pages() {
    struct Guarded {
        base: *mut libc::c_void,
        page: usize,
    }
    impl Drop for Guarded {
        fn drop(&mut self) {
            // SAFETY: this owns the original mmap range, and no slices survive it.
            assert_eq!(unsafe { libc::munmap(self.base, self.page * 3) }, 0);
        }
    }
    // SAFETY: sysconf has no pointer arguments and does not mutate Rust data.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    assert!(page >= 256);
    let page = page as usize;
    // SAFETY: anonymous mapping with no file descriptor; check MAP_FAILED before use.
    let base = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            page * 3,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(base, libc::MAP_FAILED);
    let mapping = Guarded { base, page };
    // SAFETY: middle points to the middle page within this owned mapping.
    let middle = unsafe { mapping.base.cast::<u8>().add(page) };
    // SAFETY: middle is page-aligned and the range is inside the owned mapping.
    assert_eq!(
        unsafe { libc::mprotect(middle.cast(), page, libc::PROT_READ | libc::PROT_WRITE) },
        0
    );
    // SAFETY: the middle page is now writable and no references alias it.
    unsafe { middle.write_bytes(b'a', page) };
    for length in 0..=256 {
        // Exercise both ends: the next or previous page is inaccessible.
        for offset in [0, page - length] {
            // SAFETY: this slice includes only initialized bytes in the readable
            // middle page (or is empty at its end), never either guard page.
            let bytes = unsafe { std::slice::from_raw_parts(middle.add(offset), length) };
            assert_eq!(simd::ascii_iri_prefix(bytes), length / 16 * 16);
        }
    }
    let line = b"<urn:abcdefghijklmnopqrstuvwxyz/abcdefghijklmnopqrstuvwxyz><urn:p><urn:o>.\n";
    // SAFETY: the line fits in the writable middle page; all prior slices expired.
    unsafe {
        std::ptr::copy_nonoverlapping(line.as_ptr(), middle.add(page - line.len()), line.len())
    };
    for length in 0..=line.len() {
        // Every prefix ends immediately before a guard page.
        // SAFETY: the copied bytes are in the readable middle page, with no aliases.
        unsafe { std::ptr::copy_nonoverlapping(line.as_ptr(), middle.add(page - length), length) };
        // SAFETY: this slice only borrows the just-initialized readable bytes.
        let bytes = unsafe { std::slice::from_raw_parts(middle.add(page - length), length) };
        agrees_with_scalar(bytes, true);
        agrees_with_scalar(bytes, false);
    }
}

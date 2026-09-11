//! Place this at examples/differential.rs in the prototype checkout and copy
//! the unmodified tokenizer to examples/reference/mod.rs before building.
use s_parser::{DecodeError, Decoded, LineTokenizer, TermType, Token, Tokenizer};
#[path = "reference/mod.rs"]
mod reference;
fn check(bytes: &[u8], eof: bool) {
    let a = reference::NtTokenizer.decode(bytes, eof);
    let b = s_parser::NtTokenizer.decode(bytes, eof);
    match (a, b) {
        (Ok(a), Ok(b)) => {
            assert_eq!(a.consumed, b.consumed);
            assert_eq!(a.tokens, b.tokens);
        }
        (Err(a), Err(b)) => assert_eq!(a, b),
        (a, b) => panic!("different result at eof={eof} for {bytes:?}: {a:?} / {b:?}"),
    }
}
fn main() {
    let input = b"<https://example.org/abcdefghijklmnopqrstuvwxyz/abcdefghijklmnopqrstuvwxyz/abcdefghijklmnopqrstuvwxyz> <urn:predicate> <https://example.org/abcdefghijklmnopqrstuvwxyz/abcdefghijklmnopqrstuvwxyz> .\n";
    let mut cases = 0;
    for position in 0..input.len() {
        for value in 0..=255u8 {
            let mut bytes = input.to_vec();
            bytes[position] = value;
            check(&bytes, true);
            check(&bytes, false);
            cases += 2;
        }
    }
    // Every possible window end, including inside a vector, escape, or UTF-8.
    for data in [input.as_slice(), "<https://example.org/abcdefghijklmnop/é/🦀/\\u0041/abcdefghijklmnop> <urn:p> <urn:o>.\n".as_bytes()] {
        for end in 0..=data.len() {
            check(&data[..end], true);
            check(&data[..end], false);
            cases += 2;
        }
    }
    println!("{cases} differential cases agree, including error offsets");
}

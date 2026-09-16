//! Allocation-free N-Triples lexical scanner. Escapes are validated and retained.
use super::{DecodeError, Decoded, LineTokenizer, Tokenizer};
use crate::rdf::{TermType, Token};

mod simd;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Default)]
pub struct NtTokenizer;
impl LineTokenizer for NtTokenizer {}

impl Tokenizer for NtTokenizer {
    type Output = [Token; 4];
    fn decode(&mut self, bytes: &[u8], eof: bool) -> Result<Decoded> {
        decode::<true>(bytes, eof)
    }
    fn bounds_ok(&self, output: &[Token; 4], consumed: usize) -> bool {
        output.iter().all(|t| t.end() <= consumed)
    }
}

// A compile-time switch lets tests compare the SIMD path to the same grammar
// with scalar scanning, without adding a branch or a public configuration flag.
fn decode<const SIMD: bool>(bytes: &[u8], eof: bool) -> Result<Decoded> {
    if bytes.len() > u32::MAX as usize {
        return Err(DecodeError::Invalid {
            offset: 0,
            message: "record exceeds 32-bit token offsets",
        });
    }
    let mut s = Scanner::<SIMD> { bytes, pos: 0, eof };
    s.space();
    let tokens = match s.peek() {
        Some(b'#' | b'\r' | b'\n') => None,
        None if eof => None,
        None => return Err(DecodeError::NeedMore),
        _ => {
            let subject = s.resource()?;
            s.space();
            let predicate = s.iri()?;
            s.space();
            let object = if s.peek() == Some(b'"') {
                s.literal()?
            } else {
                s.resource()?
            };
            s.space();
            s.expect(b'.')?;
            Some([subject, predicate, object, Token::default()])
        }
    };
    s.space();
    if s.peek() == Some(b'#') {
        while s.peek().is_some_and(|b| b != b'\r' && b != b'\n') {
            s.pos += 1;
        }
    }
    match s.peek() {
        // Treat each CR/LF as a boundary; CRLF's LF is a harmless blank line.
        // This avoids lookahead beyond the configured record budget.
        Some(b'\r' | b'\n') => s.pos += 1,
        None if eof => (),
        None => return Err(DecodeError::NeedMore),
        _ => return s.invalid("expected end of line after triple"),
    }
    if let Err(error) = std::str::from_utf8(&bytes[..s.pos]) {
        return Err(DecodeError::Invalid {
            offset: error.valid_up_to(),
            message: "invalid UTF-8",
        });
    }
    Ok(Decoded {
        tokens,
        consumed: s.pos,
    })
}

struct Scanner<'a, const SIMD: bool> {
    bytes: &'a [u8],
    pos: usize,
    eof: bool,
}
type Result<T> = std::result::Result<T, DecodeError>;

impl<const SIMD: bool> Scanner<'_, SIMD> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }
    fn invalid<T>(&self, message: &'static str) -> Result<T> {
        Err(DecodeError::Invalid {
            offset: self.pos,
            message,
        })
    }
    fn missing<T>(&self) -> Result<T> {
        if self.eof {
            self.invalid("unexpected end of input")
        } else {
            Err(DecodeError::NeedMore)
        }
    }
    fn take(&mut self) -> Result<u8> {
        let Some(b) = self.peek() else {
            return self.missing();
        };
        self.pos += 1;
        Ok(b)
    }
    fn expect(&mut self, b: u8) -> Result<()> {
        match self.peek() {
            Some(c) if c == b => {
                self.pos += 1;
                Ok(())
            }
            None => self.missing(),
            _ => self.invalid("unexpected character"),
        }
    }
    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }
    fn token(&self, kind: TermType, start: usize, value_end: usize) -> Token {
        Token::new(kind, start as u32, value_end as u32, self.pos as u32).unwrap()
    }
    fn resource(&mut self) -> Result<Token> {
        match self.peek() {
            Some(b'<') => self.iri(),
            Some(b'_') => self.blank(),
            None => self.missing(),
            _ => self.invalid("expected IRI or blank node"),
        }
    }
    fn iri(&mut self) -> Result<Token> {
        let start = self.pos;
        self.expect(b'<')?;
        let mut scheme_len = 0;
        let mut absolute = false;
        loop {
            if SIMD && absolute {
                self.pos += simd::ascii_iri_prefix(&self.bytes[self.pos..]);
            }
            let b = match self.peek() {
                Some(b) => b,
                None => return self.missing(),
            };
            if b == b'>' {
                if !absolute {
                    return self.invalid("expected absolute IRI");
                }
                let value_end = self.pos;
                self.pos += 1;
                return Ok(self.token(TermType::Iri, start, value_end));
            }
            let c = if b == b'\\' {
                self.escape(false)?
            } else {
                if b <= 0x20 || matches!(b, b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`') {
                    return self.invalid("forbidden IRI character");
                }
                self.pos += 1;
                b as u32
            };
            if !absolute {
                if c == b':' as u32 && scheme_len != 0 {
                    absolute = true;
                } else if c <= 127
                    && ((c as u8).is_ascii_alphabetic()
                        || (scheme_len != 0
                            && ((c as u8).is_ascii_digit()
                                || matches!(c as u8, b'+' | b'-' | b'.'))))
                {
                    scheme_len += 1;
                } else {
                    return self.invalid("invalid absolute IRI scheme");
                }
            }
        }
    }
    fn escape(&mut self, literal: bool) -> Result<u32> {
        self.expect(b'\\')?;
        let b = self.take()?;
        let digits = match b {
            b'u' => 4,
            b'U' => 8,
            b't' | b'b' | b'n' | b'r' | b'f' | b'"' | b'\'' | b'\\' if literal => {
                return Ok(b as u32);
            }
            _ => return self.invalid("invalid escape"),
        };
        let mut value = 0u32;
        for _ in 0..digits {
            let b = self.take()?;
            let Some(digit) = (b as char).to_digit(16) else {
                return self.invalid("expected hexadecimal digit");
            };
            value = (value << 4) | digit;
        }
        if char::from_u32(value).is_none() {
            return self.invalid("escape is not a Unicode scalar value");
        }
        Ok(value)
    }
    fn literal(&mut self) -> Result<Token> {
        let start = self.pos;
        self.expect(b'"')?;
        loop {
            match self.peek() {
                Some(b'"') => break,
                Some(b'\\') => {
                    self.escape(true)?;
                }
                Some(b'\r' | b'\n') => return self.invalid("newline inside literal"),
                Some(_) => self.pos += 1,
                None => return self.missing(),
            }
        }
        let value_end = self.pos;
        self.pos += 1;
        let kind = match self.peek() {
            Some(b'@') => {
                self.pos += 1;
                let lang_start = self.pos;
                while self.peek().is_some_and(|b| b.is_ascii_alphabetic()) {
                    self.pos += 1;
                }
                if self.pos == lang_start {
                    if self.peek().is_none() {
                        return self.missing();
                    }
                    return self.invalid("empty language tag");
                }
                while self.peek() == Some(b'-') {
                    self.pos += 1;
                    let part = self.pos;
                    while self.peek().is_some_and(|b| b.is_ascii_alphanumeric()) {
                        self.pos += 1;
                    }
                    if self.pos == part {
                        if self.peek().is_none() {
                            return self.missing();
                        }
                        return self.invalid("empty language subtag");
                    }
                }
                TermType::LanguageLiteral
            }
            Some(b'^') => {
                self.pos += 1;
                self.expect(b'^')?;
                self.iri()?;
                TermType::TypedLiteral
            }
            None if !self.eof => return Err(DecodeError::NeedMore),
            _ => TermType::Literal,
        };
        Ok(self.token(kind, start, value_end))
    }
    fn character(&self) -> Result<char> {
        let Some(b) = self.peek() else {
            return self.missing();
        };
        if b < 128 {
            return Ok(b as char);
        }
        let width = match b {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return self.invalid("invalid UTF-8"),
        };
        let Some(bytes) = self.bytes.get(self.pos..self.pos + width) else {
            return self.missing();
        };
        match std::str::from_utf8(bytes) {
            Ok(s) => Ok(s.chars().next().unwrap()),
            Err(_) => self.invalid("invalid UTF-8"),
        }
    }
    fn blank(&mut self) -> Result<Token> {
        let start = self.pos;
        self.expect(b'_')?;
        self.expect(b':')?;
        let c = self.character()?;
        if !(pn_base(c) || c == '_' || c.is_ascii_digit()) {
            return self.invalid("invalid blank node label");
        }
        self.pos += c.len_utf8();
        let mut value_end = self.pos;
        loop {
            if self.peek().is_none() {
                if !self.eof {
                    return Err(DecodeError::NeedMore);
                }
                break;
            }
            let c = self.character()?;
            if c == '.' {
                self.pos += 1;
            } else if pn_base(c)
                || c == '_'
                || c == '-'
                || c.is_ascii_digit()
                || matches!(c as u32, 0xB7 | 0x300..=0x36F | 0x203F..=0x2040)
            {
                self.pos += c.len_utf8();
                value_end = self.pos;
            } else {
                break;
            }
        }
        // Trailing dots belong to the statement terminator, not the label.
        self.pos = value_end;
        Ok(self.token(TermType::BlankNode, start, value_end))
    }
}

fn pn_base(c: char) -> bool {
    c.is_ascii_alphabetic()
        || matches!(c as u32,
        0xC0..=0xD6 | 0xD8..=0xF6 | 0xF8..=0x2FF | 0x370..=0x37D |
        0x37F..=0x1FFF | 0x200C..=0x200D | 0x2070..=0x218F | 0x2C00..=0x2FEF |
        0x3001..=0xD7FF | 0xF900..=0xFDCF | 0xFDF0..=0xFFFD | 0x10000..=0xEFFFF)
}

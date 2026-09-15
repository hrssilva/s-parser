//! Lexical primitives for SPARQL query syntax, driven directly by the
//! recursive-descent parser in [`super::parser`] rather than pre-tokenized
//! into an intermediate stream — the same style [`crate::tokenizer::nt_tokenizer`]
//! uses, and for the same reason: which alternative a byte like `(` or `[`
//! starts depends entirely on grammar position, so a context-free token
//! stream would just be re-interpreted by the parser anyway. Unlike
//! N-Triples, SPARQL documents are small (query text, not bulk data), so
//! there is no SIMD path here: scalar scanning is already far from the
//! bottleneck for KB-sized input.
use crate::sparql::term::{NumKind, Span};
use crate::tokenizer::DecodeError;

pub(super) type Result<T> = std::result::Result<T, DecodeError>;

pub(super) struct Scanner<'a> {
    pub bytes: &'a [u8],
    pub pos: usize,
    pub eof: bool,
}

impl<'a> Scanner<'a> {
    pub fn new(bytes: &'a [u8], eof: bool) -> Self {
        Self { bytes, pos: 0, eof }
    }
    pub fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }
    pub fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }
    pub fn invalid<T>(&self, message: &'static str) -> Result<T> {
        Err(DecodeError::Invalid {
            offset: self.pos,
            message,
        })
    }
    pub fn missing<T>(&self) -> Result<T> {
        if self.eof {
            self.invalid("unexpected end of input")
        } else {
            Err(DecodeError::NeedMore)
        }
    }
    pub fn take(&mut self) -> Result<u8> {
        let Some(b) = self.peek() else {
            return self.missing();
        };
        self.pos += 1;
        Ok(b)
    }
    pub fn expect(&mut self, b: u8) -> Result<()> {
        match self.peek() {
            Some(c) if c == b => {
                self.pos += 1;
                Ok(())
            }
            None => self.missing(),
            _ => self.invalid("unexpected character"),
        }
    }
    /// Skip whitespace and `#`-to-end-of-line comments. Never returns
    /// `NeedMore`: running out of bytes mid-skip just means "nothing more to
    /// skip yet," which the caller (always re-checking `peek()`) handles.
    pub fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.pos += 1,
                Some(b'#') => {
                    while self.peek().is_some_and(|b| b != b'\r' && b != b'\n') {
                        self.pos += 1;
                    }
                }
                _ => return,
            }
        }
    }
    /// One Unicode scalar value at the current position, without advancing.
    pub fn character(&self) -> Result<char> {
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

    // -- IRIREF, no escapes in SPARQL (unlike Turtle/N-Triples) --
    pub fn iriref(&mut self) -> Result<Span> {
        self.expect(b'<')?;
        let start = self.pos;
        loop {
            match self.peek() {
                None => return self.missing(),
                Some(b'>') => {
                    let end = self.pos;
                    self.pos += 1;
                    return Ok(Span::new(start as u32, end as u32));
                }
                Some(b)
                    if b <= 0x20
                        || matches!(b, b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`' | b'\\') =>
                {
                    return self.invalid("forbidden IRIREF character");
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    // -- STRING_LITERAL{1,2,LONG1,LONG2}; content span excludes delimiters. --
    pub fn string_literal(&mut self) -> Result<Span> {
        let quote = self.take()?;
        let long = self.peek() == Some(quote) && self.peek_at(1) == Some(quote);
        if long {
            self.pos += 2;
        }
        let start = self.pos;
        loop {
            match self.peek() {
                None => return self.missing(),
                Some(b'\\') => {
                    self.pos += 1;
                    self.echar()?;
                }
                Some(b'\r' | b'\n') if !long => {
                    return self.invalid("newline inside string literal");
                }
                Some(b) if b == quote => {
                    if !long {
                        let end = self.pos;
                        self.pos += 1;
                        return Ok(Span::new(start as u32, end as u32));
                    }
                    if self.peek_at(1) == Some(quote) && self.peek_at(2) == Some(quote) {
                        let end = self.pos;
                        self.pos += 3;
                        return Ok(Span::new(start as u32, end as u32));
                    }
                    self.pos += 1;
                }
                Some(_) => self.pos += 1,
            }
        }
    }
    fn echar(&mut self) -> Result<()> {
        let b = self.take()?;
        if matches!(b, b't' | b'b' | b'n' | b'r' | b'f' | b'"' | b'\'' | b'\\') {
            Ok(())
        } else {
            self.invalid("invalid string escape")
        }
    }

    /// `@tag(-subtag)*`; span excludes the `@`. Caller has already peeked `@`.
    pub fn langtag(&mut self) -> Result<Span> {
        self.expect(b'@')?;
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        if self.pos == start {
            return self.missing_or_invalid("empty language tag");
        }
        while self.peek() == Some(b'-') {
            self.pos += 1;
            let part = self.pos;
            while self.peek().is_some_and(|b| b.is_ascii_alphanumeric()) {
                self.pos += 1;
            }
            if self.pos == part {
                return self.missing_or_invalid("empty language subtag");
            }
        }
        Ok(Span::new(start as u32, self.pos as u32))
    }
    fn missing_or_invalid<T>(&self, message: &'static str) -> Result<T> {
        if self.peek().is_none() && !self.eof {
            Err(DecodeError::NeedMore)
        } else {
            self.invalid(message)
        }
    }

    /// `?name`/`$name`; span excludes the sigil. Caller has peeked `?`/`$`.
    pub fn var(&mut self) -> Result<Span> {
        self.pos += 1;
        let start = self.pos;
        if !self.varname_step(true)? {
            return self.invalid("expected variable name");
        }
        loop {
            if self.peek().is_none() {
                if !self.eof {
                    return Err(DecodeError::NeedMore);
                }
                break;
            }
            if !self.varname_step(false)? {
                break;
            }
        }
        Ok(Span::new(start as u32, self.pos as u32))
    }
    fn varname_step(&mut self, first: bool) -> Result<bool> {
        match self.peek() {
            Some(b) if b.is_ascii_digit() => {
                self.pos += 1;
                Ok(true)
            }
            Some(_) => {
                let c = self.character()?;
                if pn_chars_u(c)
                    || (!first && matches!(c as u32, 0xB7 | 0x300..=0x36F | 0x203F..=0x2040))
                {
                    self.pos += c.len_utf8();
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            None if self.eof => Ok(false),
            None => Err(DecodeError::NeedMore),
        }
    }

    /// `_:label`; span excludes the `_:` marker. Caller has peeked `_`.
    pub fn blank_node_label(&mut self) -> Result<Span> {
        self.expect(b'_')?;
        self.expect(b':')?;
        let start = self.pos;
        let c = self.character()?;
        if !(pn_chars_base(c) || c == '_' || c.is_ascii_digit()) {
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
            } else if pn_chars_base(c) || pn_chars_extra(c) {
                self.pos += c.len_utf8();
                value_end = self.pos;
            } else {
                break;
            }
        }
        self.pos = value_end;
        Ok(Span::new(start as u32, value_end as u32))
    }

    /// A maximal `PN_PREFIX`-shaped run at the current position, without
    /// consuming it: the caller decides (by checking for an immediately
    /// following `:`, with no whitespace) whether this is a namespace
    /// prefix or a bare keyword/identifier, then commits by calling
    /// [`Self::advance_to`].
    pub fn peek_word(&self) -> Option<Span> {
        let start = self.pos;
        let mut p = self.pos;
        let c = str_char_at(self.bytes, p)?;
        if !pn_chars_base(c) {
            return None;
        }
        p += c.len_utf8();
        let mut value_end = p;
        while let Some(c) = str_char_at(self.bytes, p) {
            if c == '.' {
                p += 1;
            } else if pn_chars_base(c) || pn_chars_extra(c) {
                p += c.len_utf8();
                value_end = p;
            } else {
                break;
            }
        }
        Some(Span::new(start as u32, value_end as u32))
    }
    pub fn advance_to(&mut self, pos: usize) {
        self.pos = pos;
    }
    /// `true` if a word ends here but we can't yet tell whether more of it
    /// is waiting in the next window (only relevant at the buffer's end).
    pub fn word_needs_more(&self, span: Span) -> bool {
        span.end as usize == self.bytes.len() && !self.eof
    }

    /// Cheap, non-consuming check for whether `pn_local` would match here —
    /// used to tell a bare `prefix:` (empty local) apart from `prefix:local`
    /// without committing to a full scan. Precise Unicode classification is
    /// left to `pn_local` itself; a non-ASCII lead byte is optimistically
    /// accepted here and validated for real there.
    pub fn at_pn_local_start(&self) -> Result<bool> {
        match self.peek() {
            Some(b'%' | b'\\' | b':') => Ok(true),
            Some(b) => Ok(b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80),
            None if self.eof => Ok(false),
            None => Err(DecodeError::NeedMore),
        }
    }

    /// `PN_LOCAL` after a confirmed `prefix:`. A caller-supplied first-char
    /// check is intentionally not stricter than the continuation set — a
    /// pragmatic widening, not a spec-strict scan; see the crate's stance on
    /// staying a lexical layer rather than validating every grammar nuance.
    pub fn pn_local(&mut self) -> Result<Span> {
        let start = self.pos;
        if !self.pn_local_step()? {
            return self.invalid("expected local name");
        }
        let mut value_end = self.pos;
        loop {
            if self.peek().is_none() {
                if !self.eof {
                    return Err(DecodeError::NeedMore);
                }
                break;
            }
            if self.peek() == Some(b'.') {
                self.pos += 1;
            } else if self.pn_local_step()? {
                value_end = self.pos;
            } else {
                break;
            }
        }
        self.pos = value_end;
        Ok(Span::new(start as u32, value_end as u32))
    }
    fn pn_local_step(&mut self) -> Result<bool> {
        match self.peek() {
            Some(b'%') => {
                self.pos += 1;
                self.hex_digit()?;
                self.hex_digit()?;
                Ok(true)
            }
            Some(b'\\') => {
                self.pos += 1;
                let b = self.take()?;
                if matches!(
                    b,
                    b'_' | b'~'
                        | b'.'
                        | b'-'
                        | b'!'
                        | b'$'
                        | b'&'
                        | b'\''
                        | b'('
                        | b')'
                        | b'*'
                        | b'+'
                        | b','
                        | b';'
                        | b'='
                        | b'/'
                        | b'?'
                        | b'#'
                        | b'@'
                        | b'%'
                ) {
                    Ok(true)
                } else {
                    self.invalid("invalid local name escape")
                }
            }
            Some(b':') => {
                self.pos += 1;
                Ok(true)
            }
            None if self.eof => Ok(false),
            None => Err(DecodeError::NeedMore),
            Some(_) => {
                let c = self.character()?;
                if pn_chars_base(c) || pn_chars_extra(c) {
                    self.pos += c.len_utf8();
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
        }
    }
    fn hex_digit(&mut self) -> Result<()> {
        let b = self.take()?;
        if (b as char).is_ascii_hexdigit() {
            Ok(())
        } else {
            self.invalid("expected hexadecimal digit")
        }
    }

    /// Tries to scan a (possibly signed) numeric literal at the current
    /// position. Returns `Ok(None)` without consuming anything if the
    /// current byte can't start one, so the caller can fall through to
    /// another primary-expression alternative — including treating a bare
    /// `+`/`-` as the additive operator, per the grammar's own lexer note on
    /// `NumericLiteralPositive`/`NumericLiteralNegative` immediately
    /// following an operand with no separating whitespace.
    pub fn numeric_literal(&mut self) -> Result<Option<(NumKind, Span)>> {
        let start = self.pos;
        let mut p = self.pos;
        if matches!(self.bytes.get(p), Some(b'+' | b'-')) {
            p += 1;
        }
        let digits_start = p;
        if !matches!(self.bytes.get(p), Some(b) if b.is_ascii_digit() || *b == b'.') {
            return Ok(None);
        }
        self.pos = p;
        let int_digits = self.scan_digits()?;
        let mut frac_digits = 0u32;
        let mut has_dot = false;
        if self.peek() == Some(b'.') {
            has_dot = true;
            self.pos += 1;
            frac_digits = self.scan_digits()?;
        }
        let has_exp = matches!(self.peek(), Some(b'e' | b'E'));
        if has_exp {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exp_digits = self.scan_digits()?;
            if exp_digits == 0 {
                return self.invalid("expected exponent digits");
            }
        }
        let kind = if has_exp {
            NumKind::Double
        } else if has_dot {
            if frac_digits == 0 {
                return self.invalid("expected digits after decimal point");
            }
            NumKind::Decimal
        } else {
            debug_assert!(int_digits > 0 || digits_start == p);
            NumKind::Integer
        };
        Ok(Some((kind, Span::new(start as u32, self.pos as u32))))
    }
    fn scan_digits(&mut self) -> Result<u32> {
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == self.bytes.len() && !self.eof {
            return Err(DecodeError::NeedMore);
        }
        Ok((self.pos - start) as u32)
    }

    /// `NIL` (`'(' WS* ')'`): consumes and returns `true` only on a full
    /// match; otherwise consumes nothing.
    pub fn try_nil(&mut self) -> Result<bool> {
        if self.peek() != Some(b'(') {
            return Ok(false);
        }
        let mut p = self.pos + 1;
        loop {
            match self.bytes.get(p) {
                Some(b' ' | b'\t' | b'\r' | b'\n') => p += 1,
                Some(b')') => {
                    self.pos = p + 1;
                    return Ok(true);
                }
                Some(_) => return Ok(false),
                None => {
                    if self.eof {
                        return Ok(false);
                    }
                    return Err(DecodeError::NeedMore);
                }
            }
        }
    }
    /// `ANON` (`'[' WS* ']'`): consumes and returns `true` only on a full match.
    pub fn try_anon(&mut self) -> Result<bool> {
        if self.peek() != Some(b'[') {
            return Ok(false);
        }
        let mut p = self.pos + 1;
        loop {
            match self.bytes.get(p) {
                Some(b' ' | b'\t' | b'\r' | b'\n') => p += 1,
                Some(b']') => {
                    self.pos = p + 1;
                    return Ok(true);
                }
                Some(_) => return Ok(false),
                None => {
                    if self.eof {
                        return Ok(false);
                    }
                    return Err(DecodeError::NeedMore);
                }
            }
        }
    }
}

fn str_char_at(bytes: &[u8], pos: usize) -> Option<char> {
    let b = *bytes.get(pos)?;
    if b < 128 {
        return Some(b as char);
    }
    let width = match b {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return None,
    };
    std::str::from_utf8(bytes.get(pos..pos + width)?)
        .ok()
        .and_then(|s| s.chars().next())
}

pub(super) fn pn_chars_base(c: char) -> bool {
    c.is_ascii_alphabetic()
        || matches!(c as u32,
        0xC0..=0xD6 | 0xD8..=0xF6 | 0xF8..=0x2FF | 0x370..=0x37D |
        0x37F..=0x1FFF | 0x200C..=0x200D | 0x2070..=0x218F | 0x2C00..=0x2FEF |
        0x3001..=0xD7FF | 0xF900..=0xFDCF | 0xFDF0..=0xFFFD | 0x10000..=0xEFFFF)
}
pub(super) fn pn_chars_u(c: char) -> bool {
    pn_chars_base(c) || c == '_'
}
/// The rest of `PN_CHARS` beyond `PN_CHARS_U`: `-`, digits, and the
/// mid-name combining/joiner ranges.
pub(super) fn pn_chars_extra(c: char) -> bool {
    pn_chars_u(c)
        || c == '-'
        || c.is_ascii_digit()
        || matches!(c as u32, 0xB7 | 0x300..=0x36F | 0x203F..=0x2040)
}

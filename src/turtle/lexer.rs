//! Lexical primitives for Turtle syntax, driven directly by the parser in
//! [`super::parser`] — the same style `sparql::lexer`/`nt_tokenizer` use,
//! for the same reason. Turtle's terminal grammar mostly matches SPARQL's
//! (`PN_PREFIX`/`PN_LOCAL`/`BLANK_NODE_LABEL`/numeric literals), so this
//! mirrors `sparql::lexer` closely, except: `IRIREF` and string literals
//! both allow `\uXXXX`/`\UXXXXXXXX` (`UCHAR`) escapes here — unlike SPARQL,
//! which forbids escapes in `IRIREF` entirely and allows only `ECHAR` in
//! strings — matching `nt_tokenizer`'s escape handling instead.
use crate::tokenizer::DecodeError;
use crate::turtle::term::{NumKind, Span};

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
    /// Skip whitespace and `#`-to-end-of-line comments.
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

    /// `UCHAR ::= '\u' HEX HEX HEX HEX | '\U' HEX HEX HEX HEX HEX HEX HEX HEX`.
    /// Caller has peeked `\`. Only validates (a numeric escape's *presence*
    /// in the record doesn't change its written form, unlike `ECHAR` which
    /// is validated purely for well-formedness too); this stays a lexical
    /// layer and doesn't decode the escape into the codepoint it names.
    fn uchar(&mut self) -> Result<()> {
        self.expect(b'\\')?;
        let b = self.take()?;
        let digits = match b {
            b'u' => 4,
            b'U' => 8,
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
        Ok(())
    }

    // -- IRIREF, UCHAR escapes only (no ECHAR) --
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
                Some(b'\\') => self.uchar()?,
                Some(b)
                    if b <= 0x20 || matches!(b, b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`') =>
                {
                    return self.invalid("forbidden IRIREF character");
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    // -- STRING_LITERAL{1,2,LONG1,LONG2}: both ECHAR and UCHAR. --
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
                Some(b'\\') => self.string_escape()?,
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
    fn string_escape(&mut self) -> Result<()> {
        match self.peek_at(1) {
            Some(b'u' | b'U') => self.uchar(),
            _ => {
                self.expect(b'\\')?;
                let b = self.take()?;
                if matches!(b, b't' | b'b' | b'n' | b'r' | b'f' | b'"' | b'\'' | b'\\') {
                    Ok(())
                } else {
                    self.invalid("invalid string escape")
                }
            }
        }
    }

    /// `@tag(-subtag)*`; span excludes the `@`. Caller has peeked `@`.
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
    /// consuming it — see `sparql::lexer::Scanner::peek_word`, which this
    /// matches exactly, including the bare-`:` (empty prefix) case.
    pub fn peek_word(&self) -> Option<Span> {
        let start = self.pos;
        if self.bytes.get(start) == Some(&b':') {
            return Some(Span::new(start as u32, start as u32));
        }
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
    pub fn word_needs_more(&self, span: Span) -> bool {
        span.end as usize == self.bytes.len() && !self.eof
    }
    pub fn at_pn_local_start(&self) -> Result<bool> {
        match self.peek() {
            Some(b'%' | b'\\' | b':') => Ok(true),
            Some(b) => Ok(b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80),
            None if self.eof => Ok(false),
            None => Err(DecodeError::NeedMore),
        }
    }

    /// `PN_LOCAL` after a confirmed `prefix:` — see
    /// `sparql::lexer::Scanner::pn_local`, matched exactly.
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
            None => Ok(false),
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

    /// A (possibly signed) `INTEGER`/`DECIMAL`/`DOUBLE`. Turtle numeric
    /// literals aren't expressions, so — unlike SPARQL's — there's no
    /// "immediately follows an operand" ambiguity to resolve: a leading
    /// sign is simply part of the literal whenever one is written.
    pub fn numeric_literal(&mut self) -> Result<Option<(NumKind, Span)>> {
        let start = self.pos;
        let mut p = self.pos;
        if matches!(self.bytes.get(p), Some(b'+' | b'-')) {
            p += 1;
        }
        match self.bytes.get(p) {
            Some(b) if b.is_ascii_digit() || *b == b'.' => {}
            Some(_) => return Ok(None),
            // A lone sign right at the window's end: more input could still
            // turn this into a number (or rule it out) — the same
            // "never guess this close to a boundary" contract `scan_digits`
            // enforces below.
            None if p > self.pos && !self.eof => return Err(DecodeError::NeedMore),
            None => return Ok(None),
        }
        self.pos = p;
        let int_digits = self.scan_digits()?;
        let mut has_dot = false;
        // `DECIMAL ::= [0-9]* '.' [0-9]+` needs at least one fraction digit;
        // `DOUBLE`'s `[0-9]+ '.' [0-9]* EXPONENT` allows zero only when an
        // EXPONENT follows. So a `.` is only part of the number when a digit
        // or an exponent marker follows it — otherwise it's some other
        // token (typically the statement-terminating `.`), not a decimal
        // point: `123.` is the integer `123` followed by `.`, not a
        // truncated decimal. At a window boundary where what follows the
        // `.` isn't known yet, more input is needed before deciding either
        // way, rather than guessing.
        if self.peek() == Some(b'.') {
            match self.peek_at(1) {
                Some(b) if b.is_ascii_digit() || matches!(b, b'e' | b'E') => {
                    has_dot = true;
                    self.pos += 1;
                    self.scan_digits()?;
                }
                Some(_) => {}
                None if self.eof => {}
                None => return Err(DecodeError::NeedMore),
            }
        }
        if int_digits == 0 && !has_dot {
            // Neither an integer digit run nor a decimal point was actually
            // consumed (e.g. a lone '.' not followed by a digit or
            // exponent): not a numeric literal at all.
            self.pos = start;
            return Ok(None);
        }
        let has_exp = matches!(self.peek(), Some(b'e' | b'E'));
        if has_exp {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.scan_digits()? == 0 {
                return self.invalid("expected exponent digits");
            }
        }
        let kind = if has_exp {
            NumKind::Double
        } else if has_dot {
            NumKind::Decimal
        } else {
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

    /// `NIL` (`'(' WS* ')'`): consumes and returns `true` only on a full match.
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
fn pn_chars_u(c: char) -> bool {
    pn_chars_base(c) || c == '_'
}
pub(super) fn pn_chars_extra(c: char) -> bool {
    pn_chars_u(c)
        || c == '-'
        || c.is_ascii_digit()
        || matches!(c as u32, 0xB7 | 0x300..=0x36F | 0x203F..=0x2040)
}

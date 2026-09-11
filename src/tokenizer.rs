//! Token offsets are relative to one record, never to the entire file.
pub mod nt_tokenizer;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum TermType {
    #[default]
    DefaultGraph,
    Iri,
    BlankNode,
    Literal,
    LanguageLiteral,
    TypedLiteral,
}

/// Four tokens occupy 64 bytes. Lexical values retain escapes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Token {
    kind: TermType,
    start: u32,
    value_end: u32,
    end: u32,
}

impl Token {
    /// Offsets include the opener at `start`, exclude the closing delimiter at
    /// `value_end`, and include any language/datatype suffix up to `end`.
    pub fn new(kind: TermType, start: u32, value_end: u32, end: u32) -> Option<Self> {
        let token = Self {
            kind,
            start,
            value_end,
            end,
        };
        let closer = u32::from(matches!(
            kind,
            TermType::Iri | TermType::Literal | TermType::LanguageLiteral | TermType::TypedLiteral
        ));
        (start
            .checked_add(token.opener())
            .is_some_and(|v| v <= value_end)
            && value_end.checked_add(closer).is_some_and(|v| v <= end))
        .then_some(token)
    }
    fn opener(self) -> u32 {
        match self.kind {
            TermType::DefaultGraph => 0,
            TermType::BlankNode => 2,
            _ => 1,
        }
    }
    pub fn kind(self) -> TermType {
        self.kind
    }
    pub fn start(self) -> usize {
        self.start as usize
    }
    pub fn end(self) -> usize {
        self.end as usize
    }
    pub fn text(self, record: &[u8]) -> Option<&[u8]> {
        record.get(self.start as usize..self.end as usize)
    }
    pub fn value(self, record: &[u8]) -> Option<&[u8]> {
        record.get((self.start + self.opener()) as usize..self.value_end as usize)
    }
    /// Written suffix (`@en` or `^^<...>`), empty for other terms.
    pub fn suffix(self, record: &[u8]) -> Option<&[u8]> {
        let start = match self.kind {
            TermType::LanguageLiteral | TermType::TypedLiteral => self.value_end + 1,
            _ => self.end,
        };
        record.get(start as usize..self.end as usize)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    NeedMore,
    Invalid {
        offset: usize,
        message: &'static str,
    },
}

/// `None` tokens denote a consumed comment or blank line. `consumed` must be
/// positive and includes the line terminator (if present).
#[derive(Clone, Copy, Debug)]
pub struct Decoded<R = [Token; 4]> {
    pub tokens: Option<R>,
    pub consumed: usize,
}

pub trait Tokenizer {
    /// Compact metadata produced for a record; the reader interprets its shape.
    type Output;
    /// Decode one record. Return NeedMore only if a larger window could help;
    /// `eof` says this slice ends at the physical end of the source. NeedMore
    /// must leave record state unchanged: the reader retries the same prefix.
    fn decode(&mut self, bytes: &[u8], eof: bool) -> Result<Decoded<Self::Output>, DecodeError>;
}

/// Opt-in for formats whose records cannot contain raw CR or LF. This enables
/// independent readers to align arbitrary byte partitions without coordination.
pub trait LineTokenizer: Tokenizer {}

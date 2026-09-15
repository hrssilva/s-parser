//! The RDF term vocabulary shared by every concrete RDF syntax this crate
//! parses (N-Triples today; Turtle later). Kept separate from `tokenizer`
//! and `reader` so those stay about the parsing *mechanism* — bounded
//! windows, lending, record decoding — independent of any one data model,
//! and separate from `sparql`, whose `Term` describes a position in SPARQL
//! *query* syntax (a variable, a prefixed name not yet resolved, ...) rather
//! than a decoded RDF term. `rdf::Term` and `sparql::Term` are deliberately
//! different types for different things; do not conflate them.
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

/// Four tokens occupy 64 bytes. Lexical values retain escapes. Offsets are
/// relative to one record, never to the entire file — see the `tokenizer`
/// module for why.
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

/// A decoded RDF term: a `Token`'s offsets paired with the record bytes they
/// index into. Readers construct these; callers only read them.
#[derive(Clone, Copy, Debug)]
pub struct Term<'a> {
    token: Token,
    record: &'a [u8],
}
impl<'a> Term<'a> {
    pub(crate) fn new(token: Token, record: &'a [u8]) -> Self {
        Self { token, record }
    }
    pub fn kind(self) -> TermType {
        self.token.kind()
    }
    pub fn text(self) -> &'a [u8] {
        self.token.text(self.record).unwrap()
    }
    /// Serialized lexical value; escape sequences have not been unescaped.
    pub fn value(self) -> &'a [u8] {
        self.token.value(self.record).unwrap()
    }
    pub fn suffix(self) -> &'a [u8] {
        self.token.suffix(self.record).unwrap()
    }
}

//! The RDF term vocabulary shared by every concrete RDF syntax this crate
//! parses (N-Triples, Turtle). Kept separate from `tokenizer` and `reader`
//! so those stay about the parsing *mechanism* — bounded windows, lending,
//! record decoding — independent of any one data model, and separate from
//! `sparql`, whose `Term` describes a position in SPARQL *query* syntax (a
//! variable, a prefixed name not yet resolved, ...) rather than a decoded
//! RDF term. `rdf::Term` and `sparql::Term` are deliberately different
//! types for different things; do not conflate them.
//!
//! [`ResolvedTerm`] is a second, distinct kind of term living here: not a
//! reader's own lexical representation, but the shared, already-resolved
//! currency between *any* reader format and a [`crate::writer`] — see its
//! doc comment for why every format's own `Term` type needs its own
//! `resolve` method rather than reusing this one directly.
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
    /// This term, resolved for writing — see [`ResolvedTerm`]. Trivial for
    /// N-Triples: every term is already absolute/canonical, so this only
    /// ever slices `value()`/`suffix()` apart, never joins anything.
    pub fn resolve(self) -> ResolvedTerm<'a> {
        match self.kind() {
            TermType::Iri => ResolvedTerm::Iri(ResolvedIri::Direct(self.value())),
            TermType::BlankNode => ResolvedTerm::BlankNode(BlankId::Named(self.value())),
            TermType::Literal => ResolvedTerm::Literal {
                lexical: self.value(),
                suffix: ResolvedSuffix::None,
            },
            TermType::LanguageLiteral => ResolvedTerm::Literal {
                lexical: self.value(),
                // Suffix is `@tag`; drop the marker.
                suffix: ResolvedSuffix::Lang(&self.suffix()[1..]),
            },
            TermType::TypedLiteral => {
                // Suffix is `^^<...>`; drop the marker and the delimiters.
                let s = self.suffix();
                let iri = &s[3..s.len() - 1];
                ResolvedTerm::Literal {
                    lexical: self.value(),
                    suffix: ResolvedSuffix::Datatype(ResolvedIri::Direct(iri)),
                }
            }
            // Only ever placed in a `Quad`'s 4th slot, which `Quad::graph`
            // already turns into `None` before a caller ever sees it.
            TermType::DefaultGraph => unreachable!("DefaultGraph is not a term to resolve"),
        }
    }
}

/// An IRI's text, possibly split across two non-adjacent source pieces —
/// e.g. a resolved prefixed name (namespace + local) — rather than
/// concatenated into one. `parts()` gives the two slices to write (or
/// compare) in order; nothing here ever copies them together, so resolving
/// a prefixed name costs no allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedIri<'a> {
    Direct(&'a [u8]),
    Joined(&'a [u8], &'a [u8]),
}
impl<'a> ResolvedIri<'a> {
    /// The two slices to write in order to reconstruct the full IRI text
    /// (without escape removal).
    pub fn parts(self) -> (&'a [u8], &'a [u8]) {
        match self {
            Self::Direct(s) => (s, b""),
            Self::Joined(ns, local) => (ns, local),
        }
    }
}

/// A blank node's identity as resolved for writing: either the label text
/// as written in the source (`Named`), or a counter value synthesized by
/// the parser with no source spelling at all (`Fresh` — e.g. Turtle's `[]`
/// or a collection node). A [`crate::writer`] is responsible for choosing
/// output spellings that keep these two kinds from ever colliding with each
/// other (two *different* source nodes must never end up with the same
/// written label) — see `writer::nt_writer::NtWriter` for the scheme it
/// uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlankId<'a> {
    Named(&'a [u8]),
    Fresh(u32),
}

/// The datatype/language suffix of a [`ResolvedTerm::Literal`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedSuffix<'a> {
    None,
    Lang(&'a [u8]),
    Datatype(ResolvedIri<'a>),
}

/// One RDF term, fully resolved and ready to serialize: a prefixed name's
/// namespace and local part are already paired up (though still unjoined —
/// see [`ResolvedIri`]), and a lexical shorthand (Turtle's bare `42`/`true`)
/// is already paired with its implied datatype IRI, the way it would be
/// written out in full. This is the shared currency between any reader
/// format's own `Term` type — via a `resolve` method each defines for
/// itself, since only that format's `Term` knows how to expand its own
/// shorthands — and any [`crate::writer`]. A writer needing only this
/// shape, rather than each format's own richer `Term`, is what lets one
/// writer serve every source format without knowing any of their syntaxes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedTerm<'a> {
    Iri(ResolvedIri<'a>),
    BlankNode(BlankId<'a>),
    Literal {
        lexical: &'a [u8],
        suffix: ResolvedSuffix<'a>,
    },
}

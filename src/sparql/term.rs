//! Borrowed SPARQL terms. Spans index the original query text; nothing here
//! allocates or unescapes. Prefixed names keep their prefix and local spans
//! apart instead of concatenating them, so resolving one costs no allocation
//! either — see [`Prologue::resolve`].
use std::ops::Range as StdRange;

/// A byte range in the query source. Query documents are expected to be at
/// most a few hundred KiB, so `u32` offsets (as used for on-disk records
/// elsewhere in this crate) are ample here too.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}
impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }
    pub fn text(self, src: &[u8]) -> &[u8] {
        &src[self.start as usize..self.end as usize]
    }
    pub fn len(self) -> u32 {
        self.end - self.start
    }
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
    pub fn range(self) -> StdRange<usize> {
        self.start as usize..self.end as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Var {
    /// A variable written in the query (`?name`/`$name`); span excludes the sigil.
    Named(Span),
    /// A variable introduced by the parser, never written in the source: the
    /// result variable of a hoisted aggregate, or a `GROUP BY (expr AS ?v)`
    /// key that never named a fresh variable in a `SELECT *`. Distinguished
    /// from `Named` by construction, never by text, so it can never collide
    /// with a variable name the query author chose.
    Synthetic(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlankNode {
    /// `_:label`; span excludes the `_:` marker.
    Labeled(Span),
    /// `[]`, a bare `[...]` property list, or a collection node: none of
    /// these write a label, so the parser allocates one from a counter.
    Anonymous(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Iri {
    /// `<...>`; span is the content between the angle brackets. SPARQL's
    /// `IRIREF` grammar forbids backslash escapes, unlike Turtle/N-Triples.
    Full(Span),
    /// `prefix:local`. Resolving this against a [`Prologue`] never allocates;
    /// see [`Prologue::resolve`].
    Prefixed { prefix: Span, local: Span },
    /// An IRI the parser introduces itself, with no span in the source: the
    /// `a` keyword, and `rdf:first`/`rdf:rest`/`rdf:nil` from expanding
    /// collection and blank-node property-list syntax sugar (§5.4–5.5).
    WellKnown(WellKnown),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WellKnown {
    RdfType,
    RdfFirst,
    RdfRest,
    RdfNil,
}
impl WellKnown {
    pub fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::RdfType => b"http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            Self::RdfFirst => b"http://www.w3.org/1999/02/22-rdf-syntax-ns#first",
            Self::RdfRest => b"http://www.w3.org/1999/02/22-rdf-syntax-ns#rest",
            Self::RdfNil => b"http://www.w3.org/1999/02/22-rdf-syntax-ns#nil",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiteralSuffix {
    None,
    /// `@tag`; span excludes the `@`.
    Lang(Span),
    Datatype(Iri),
}

/// A quoted literal (`"..."`, `'...'`, or their triple-quoted forms). The
/// lexical span excludes delimiters and retains any `\`-escapes as written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RdfLiteral {
    pub lexical: Span,
    pub suffix: LiteralSuffix,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumKind {
    Integer,
    Decimal,
    Double,
}

/// A SPARQL term: an RDF term, a numeric/boolean literal shorthand, or a
/// variable. Reused across graph patterns (where `Var` is meaningful),
/// CONSTRUCT templates, and ground-term contexts (where the parser has
/// already ruled `Var` out grammatically rather than the type doing it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Term {
    Iri(Iri),
    Blank(BlankNode),
    Literal(RdfLiteral),
    /// `42`, `3.14`, `1.0e10`, or their signed forms. Span includes a
    /// leading sign when the source wrote one, so the written value is
    /// always recoverable without re-deriving it from the parse.
    Numeric(NumKind, Span),
    Boolean(bool),
    Var(Var),
}

/// `BASE`/`PREFIX` declarations collected while parsing. Neither IRI
/// resolution against `base` nor `PN_LOCAL` escape removal is performed:
/// this stays a lexical layer, like the rest of the crate.
#[derive(Clone, Debug, Default)]
pub struct Prologue {
    /// Content of the `BASE <...>` IRIREF, if any.
    pub base: Option<Span>,
    /// `(prefix, namespace-iri)` pairs in declaration order; a later
    /// declaration of the same prefix shadows earlier ones on lookup.
    pub prefixes: Vec<(Span, Span)>,
}

/// The two source slices that make up a resolved `Iri::Prefixed`, handed
/// back unjoined so resolving a prefixed name never allocates. Concatenate
/// them (after removing any `PN_LOCAL` `\`-escapes from the local part, if
/// the caller cares about the exact IRI) to get the full IRI text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedIri<'s> {
    Direct(&'s [u8]),
    Joined(&'s [u8], &'s [u8]),
}
impl<'s> ResolvedIri<'s> {
    /// The two slices to write in order to reconstruct the full IRI text
    /// (without escape removal — see the type-level note).
    pub fn parts(self) -> (&'s [u8], &'s [u8]) {
        match self {
            Self::Direct(s) => (s, b""),
            Self::Joined(ns, local) => (ns, local),
        }
    }
}

impl Prologue {
    /// Resolve a prefixed name's namespace against declared `PREFIX`es.
    /// Returns `None` for an undeclared prefix. `Iri::Full` always resolves.
    pub fn resolve<'s>(&self, iri: Iri, src: &'s [u8]) -> Option<ResolvedIri<'s>> {
        match iri {
            Iri::Full(span) => Some(ResolvedIri::Direct(span.text(src))),
            Iri::WellKnown(wk) => Some(ResolvedIri::Direct(wk.as_bytes())),
            Iri::Prefixed { prefix, local } => {
                let name = prefix.text(src);
                let ns = self
                    .prefixes
                    .iter()
                    .rev()
                    .find(|(p, _)| p.text(src) == name)?
                    .1;
                Some(ResolvedIri::Joined(ns.text(src), local.text(src)))
            }
        }
    }
}

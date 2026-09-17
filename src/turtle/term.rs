//! Turtle terms: `Copy`, lifetime-free values, the same "compact record
//! metadata" role [`crate::rdf::Token`] plays for N-Triples — see this
//! crate's `turtle` module doc comment for why the shape differs from
//! `Token` (prefixed names) and from `sparql::term::Term` (no variables,
//! and prefixes resolve differently — see [`Iri::Prefixed`]).
use std::ops::Range as StdRange;

/// A byte range within one Turtle *statement* (the record this tokenizer
/// decodes) — `u32` offsets are ample for one statement, matching every
/// other span type in this crate.
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

/// The longest namespace IRI [`Iri::Prefixed`] copies inline. Real-world
/// namespace IRIs are almost always well under this
/// (`http://www.w3.org/1999/02/22-rdf-syntax-ns#` is 44 bytes); a namespace
/// that doesn't fit is a parse error (`prefix IRI too long`), the same
/// bounded-with-a-clear-error posture `window_size`/`RecordTooLong` already
/// use elsewhere in this crate, rather than growing without bound.
pub const MAX_NAMESPACE_LEN: usize = 64;

/// A namespace IRI copied inline, `Copy`, no allocation — see [`Iri::Prefixed`]
/// and this crate's `turtle` module doc comment for why a *namespace*
/// specifically (never a whole term, never local names) needs this: it's
/// declared in one `@prefix` statement and used in arbitrarily many later
/// ones, so it can't stay a span into "the current statement" the way every
/// other piece of term data in this crate does.
#[derive(Clone, Copy)]
pub struct Namespace {
    buf: [u8; MAX_NAMESPACE_LEN],
    len: u8,
}
impl Namespace {
    pub(crate) fn new(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_NAMESPACE_LEN {
            return None;
        }
        let mut buf = [0u8; MAX_NAMESPACE_LEN];
        buf[..bytes.len()].copy_from_slice(bytes);
        Some(Self {
            buf,
            len: bytes.len() as u8,
        })
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }
}
impl std::fmt::Debug for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Namespace({:?})",
            String::from_utf8_lossy(self.as_bytes())
        )
    }
}
impl PartialEq for Namespace {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}
impl Eq for Namespace {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
pub enum Iri {
    /// `<...>` written directly in this statement — a plain span, same as
    /// every other same-statement data in this crate, since (unlike a
    /// prefix) it never needs to outlive the statement that wrote it.
    Full(Span),
    /// `prefix:local`, already resolved: `namespace` is a `Copy` of the
    /// `@prefix`-declared namespace IRI (see [`Namespace`]); `local` is
    /// still a same-statement span, since the local part is always written
    /// in the statement that uses it. Resolving this to a full IRI is then
    /// just concatenating the two — see [`Self::write_to`].
    Prefixed { namespace: Namespace, local: Span },
    /// The `a` keyword, and `rdf:first`/`rdf:rest`/`rdf:nil` from expanding
    /// collection and blank-node property-list syntax sugar.
    WellKnown(WellKnown),
}
impl Iri {
    /// Appends this IRI's text to `out` — the caller-supplied buffer this
    /// crate otherwise avoids needing, required only because a resolved
    /// `Prefixed` IRI is two non-adjacent pieces (namespace + local) with
    /// nowhere contiguous to borrow a single `&[u8]` from.
    pub fn write_to(self, record: &[u8], out: &mut Vec<u8>) {
        match self {
            Self::Full(span) => out.extend_from_slice(span.text(record)),
            Self::Prefixed { namespace, local } => {
                out.extend_from_slice(namespace.as_bytes());
                out.extend_from_slice(local.text(record));
            }
            Self::WellKnown(wk) => out.extend_from_slice(wk.as_bytes()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlankNode {
    /// `_:label`; span excludes the `_:` marker.
    Labeled(Span),
    /// `[]`, a bare `[...]` property list, or a collection node: none of
    /// these write a label, so the parser allocates one from a counter —
    /// unique for the tokenizer's lifetime, not just one statement, since
    /// Turtle blank-node identity (unlike N-Triples, scoped to one parse
    /// here) must stay distinct across the whole document.
    Anonymous(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiteralSuffix {
    None,
    /// `@tag`; span excludes the `@`.
    Lang(Span),
    Datatype(Iri),
}

/// A quoted literal (`"..."`, `'...'`, or their triple-quoted forms). The
/// lexical span excludes delimiters and retains any escapes as written.
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

/// A Turtle term: an RDF term, or a numeric/boolean literal shorthand.
/// Structurally `sparql::term::Term` minus `Var` (Turtle has no variables)
/// — but not the same type: prefixed IRIs resolve differently here (see
/// [`Iri::Prefixed`]), because unlike a SPARQL query a Turtle document's
/// `@prefix` declarations are never all in scope before the data that uses
/// them, so they can't stay unresolved spans into one bounded document the
/// way `sparql::term::Iri::Prefixed` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Term {
    Iri(Iri),
    Blank(BlankNode),
    Literal(RdfLiteral),
    Numeric(NumKind, Span),
    Boolean(bool),
}
/// One decoded triple. `subject`/`predicate`/`object` spans are relative to
/// the statement (record) they came from — see [`StatementData`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TriplePattern {
    pub subject: Term,
    pub predicate: Term,
    pub object: Term,
}

/// How many triples one Turtle statement holds inline, with no allocation —
/// a tuning knob, not a correctness bound: real statements (`;`/`,`-lists,
/// `[...]`, `(...)`) are overwhelmingly small (a handful of predicates on
/// one subject), so this covers the common case for free, matching
/// [`crate::rdf::Token`]'s zero-allocation guarantee for N-Triples. A
/// statement producing more spills the *rest* into a heap `Vec` (see
/// [`StatementData`]) rather than erroring — unlike a `Stream` window (where
/// `RecordTooLong` is the right answer, since growing the window costs
/// memory proportional to the whole file), a handful of extra triples in
/// one statement is cheap and unbounded is the more honest contract: a
/// document with a long `owl:oneOf`/collection, or a manifest listing
/// dozens of entries as one subject's object list, is ordinary Turtle, not
/// a pathological input.
const INLINE_TRIPLES: usize = 16;

/// One parsed Turtle statement's triples — [`crate::tokenizer::Tokenizer::Output`]
/// for [`super::TurtleTokenizer`], playing the same "compact record
/// metadata" role `[Token; 4]` plays for one N-Triples record (and `QueryData`
/// plays for one SPARQL query). Named `...Data` rather than `Statement` for
/// the same reason `sparql::algebra::QueryData` is: the reader-facing
/// lending type — [`crate::reader::turtle_reader::Statement`] — is what
/// callers actually hold.
///
/// Holds up to [`INLINE_TRIPLES`] triples inline with no allocation
/// (`MaybeUninit`, not eagerly initialized — the earlier fixed-size,
/// `Default`-filled array this replaced paid an unconditional ~32KB
/// stack-write on every statement regardless of its actual size, which
/// measurably dominated small-statement throughput); a statement needing
/// more spills into a `Vec`, copying the already-written inline entries
/// over once, the same "small inline buffer, rare spill" trade-off a
/// `SmallVec` makes, hand-rolled here rather than pulling in that
/// dependency for one call site.
pub struct StatementData(Triples);
// The size gap between variants is the entire point: `Inline` holding its
// bytes directly, not through a `Box`, is what makes the common case
// allocation-free. Boxing it (clippy's usual suggestion here) would force a
// heap allocation on every statement, defeating the reason this type exists.
#[allow(clippy::large_enum_variant)]
enum Triples {
    Inline([std::mem::MaybeUninit<TriplePattern>; INLINE_TRIPLES], u8),
    Spilled(Vec<TriplePattern>),
}
impl StatementData {
    pub(super) fn new() -> Self {
        Self(Triples::Inline(
            [const { std::mem::MaybeUninit::uninit() }; INLINE_TRIPLES],
            0,
        ))
    }
    pub(super) fn push(&mut self, t: TriplePattern) {
        match &mut self.0 {
            Triples::Inline(buf, len) if (*len as usize) < INLINE_TRIPLES => {
                buf[*len as usize].write(t);
                *len += 1;
            }
            Triples::Inline(buf, len) => {
                let mut spilled = Vec::with_capacity(INLINE_TRIPLES * 2);
                // SAFETY: entries `0..*len` were written by the `write`
                // call above, once each, and never read before now.
                spilled.extend(
                    buf[..*len as usize]
                        .iter()
                        .map(|slot| unsafe { slot.assume_init() }),
                );
                spilled.push(t);
                self.0 = Triples::Spilled(spilled);
            }
            Triples::Spilled(v) => v.push(t),
        }
    }
    pub fn triples(&self) -> &[TriplePattern] {
        match &self.0 {
            // SAFETY: entries `0..*len` were written by `push` above, and
            // `MaybeUninit<T>` has the same layout as `T`.
            Triples::Inline(buf, len) => unsafe {
                std::slice::from_raw_parts(buf.as_ptr().cast::<TriplePattern>(), *len as usize)
            },
            Triples::Spilled(v) => v.as_slice(),
        }
    }
}
impl std::fmt::Debug for StatementData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("StatementData")
            .field(&self.triples())
            .finish()
    }
}

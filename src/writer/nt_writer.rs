//! N-Triples output — and, since N-Triples syntax is already valid Turtle,
//! "plain" Turtle output too: one `<s> <p> <o> .` line per statement, no
//! prefix substitution, no `;`/`,` grouping by subject. A prettifying
//! writer that does those is a distinct, heavier feature (it needs to
//! buffer and compare adjacent subjects, and choose prefixes) left for
//! later; this one only ever needs the one resolved statement in front of
//! it.
//!
//! [`crate::rdf::ResolvedTerm`] is the shared input: any reader format's
//! own `Term` type reaches this shape through its own `resolve` method
//! (`rdf::Term::resolve`, `turtle::term::Term::resolve`), so this writer
//! never needs to know any source syntax.
use crate::rdf::{BlankId, ResolvedIri, ResolvedSuffix, ResolvedTerm};
use std::io::{self, Write};

/// Wraps an `std::io::Write` sink. Owns no buffering beyond what the
/// underlying `W` does; wrap `out` in a `BufWriter` yourself for a file or
/// socket.
pub struct NtWriter<W> {
    out: W,
}
impl<W: Write> NtWriter<W> {
    pub fn new(out: W) -> Self {
        Self { out }
    }
    pub fn into_inner(self) -> W {
        self.out
    }
    /// Writes one statement: a triple, or (if `graph` is `Some`) an
    /// N-Quads-shaped quad. Nothing in this crate produces a graph term
    /// yet — no reader parses N-Quads' optional 4th term — but the slot
    /// costs nothing to support now, and matches `Quad::graph`'s shape for
    /// when one does.
    pub fn write_statement(
        &mut self,
        subject: ResolvedTerm<'_>,
        predicate: ResolvedTerm<'_>,
        object: ResolvedTerm<'_>,
        graph: Option<ResolvedTerm<'_>>,
    ) -> io::Result<()> {
        self.write_term(subject)?;
        self.out.write_all(b" ")?;
        self.write_term(predicate)?;
        self.out.write_all(b" ")?;
        self.write_term(object)?;
        if let Some(g) = graph {
            self.out.write_all(b" ")?;
            self.write_term(g)?;
        }
        self.out.write_all(b" .\n")
    }
    fn write_term(&mut self, term: ResolvedTerm<'_>) -> io::Result<()> {
        match term {
            ResolvedTerm::Iri(iri) => self.write_iri(iri),
            ResolvedTerm::BlankNode(id) => self.write_blank(id),
            ResolvedTerm::Literal { lexical, suffix } => self.write_literal(lexical, suffix),
        }
    }
    fn write_iri(&mut self, iri: ResolvedIri<'_>) -> io::Result<()> {
        let (a, b) = iri.parts();
        self.out.write_all(b"<")?;
        self.out.write_all(a)?;
        self.out.write_all(b)?;
        self.out.write_all(b">")
    }
    /// `Named` and `Fresh` get disjoint output prefixes (`_:l`/`_:b`): the
    /// first byte after the marker (a letter vs. a decimal digit) alone
    /// keeps the two apart, so a source label can never collide with a
    /// synthesized one even when both appear in the same document — see
    /// `rdf::BlankId`.
    fn write_blank(&mut self, id: BlankId<'_>) -> io::Result<()> {
        match id {
            BlankId::Named(label) => {
                self.out.write_all(b"_:l")?;
                self.out.write_all(label)
            }
            BlankId::Fresh(n) => {
                self.out.write_all(b"_:b")?;
                write_u32(&mut self.out, n)
            }
        }
    }
    fn write_literal(&mut self, lexical: &[u8], suffix: ResolvedSuffix<'_>) -> io::Result<()> {
        self.out.write_all(b"\"")?;
        write_nt_escaped(&mut self.out, lexical)?;
        self.out.write_all(b"\"")?;
        match suffix {
            ResolvedSuffix::None => Ok(()),
            ResolvedSuffix::Lang(tag) => {
                self.out.write_all(b"@")?;
                self.out.write_all(tag)
            }
            ResolvedSuffix::Datatype(iri) => {
                self.out.write_all(b"^^")?;
                self.write_iri(iri)
            }
        }
    }
}

/// Copies a literal's lexical bytes, escaping only what NT's
/// `STRING_LITERAL_QUOTE` forbids raw: `"`, LF, CR. A raw `\` can't occur —
/// every `\` in an already-lexed literal is guaranteed to start a validated
/// `ECHAR`/`UCHAR` escape (the lexer rejects anything else), so those are
/// always passed through unchanged, never reinterpreted. This is the one
/// place a source format's own escaping rules matter: NT and short-quoted
/// Turtle strings already forbid these raw bytes (so this is a cheap,
/// single `write_all` no-op pass for them), but Turtle's single- and
/// triple-quoted forms allow raw `"`/LF/CR that NT's single-line form
/// doesn't, and those need actual escaping here.
fn write_nt_escaped(out: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                // Skip the whole already-validated escape unchanged:
                // `ECHAR` is two bytes, `UCHAR` is `\u` + 4 hex digits or
                // `\U` + 8.
                i += 1;
                i += match bytes.get(i) {
                    Some(b'u') => 5,
                    Some(b'U') => 9,
                    _ => 1,
                };
            }
            b'"' | b'\n' | b'\r' => {
                out.write_all(&bytes[start..i])?;
                out.write_all(match bytes[i] {
                    b'"' => b"\\\"" as &[u8],
                    b'\n' => b"\\n",
                    _ => b"\\r",
                })?;
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    out.write_all(&bytes[start..])
}

/// Formats a `u32` as decimal ASCII directly into `out`, no allocation.
fn write_u32(out: &mut impl Write, mut n: u32) -> io::Result<()> {
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.write_all(&buf[i..])
}

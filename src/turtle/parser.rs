//! Recursive-descent Turtle statement parser, driven directly by
//! [`super::lexer::Scanner`] the same way `sparql::parser`/`nt_tokenizer`
//! drive their own scanners. Parses exactly one statement per call — a
//! directive (which updates the tokenizer's persistent prefix table and
//! produces no triples) or a triples statement (built directly into
//! [`StatementData`], this crate's answer to "how many triples can one
//! statement expand to": inline up to a small threshold, unbounded beyond
//! it — see that type's doc comment).
use super::lexer::Scanner;
use super::term::{
    BlankNode, Iri, LiteralSuffix, Namespace, RdfLiteral, Span, StatementData, Term, TriplePattern,
    WellKnown,
};
use crate::tokenizer::DecodeError;

pub(super) type PResult<T> = std::result::Result<T, DecodeError>;

fn word_eq_ci(bytes: &[u8], span: Span, target: &[u8]) -> bool {
    span.text(bytes).eq_ignore_ascii_case(target)
}
fn word_eq(bytes: &[u8], span: Span, target: &[u8]) -> bool {
    span.text(bytes) == target
}

/// One statement's worth of state. `prefixes`/`blank_counter` are borrowed
/// from [`super::TurtleTokenizer`] rather than owned here: unlike SPARQL's
/// `PREFIX`, Turtle's `@prefix` and blank-node identity are scoped to the
/// whole document, not to one record, so they must outlive any single
/// `Parser` — see this crate's `turtle` module doc comment.
pub(super) struct Parser<'a, 'p> {
    s: Scanner<'a>,
    prefixes: &'p mut Vec<(Vec<u8>, Vec<u8>)>,
    blank_counter: &'p mut u32,
    triples: StatementData,
}

impl<'a, 'p> Parser<'a, 'p> {
    pub fn new(
        bytes: &'a [u8],
        eof: bool,
        prefixes: &'p mut Vec<(Vec<u8>, Vec<u8>)>,
        blank_counter: &'p mut u32,
    ) -> Self {
        Self {
            s: Scanner::new(bytes, eof),
            prefixes,
            blank_counter,
            triples: StatementData::new(),
        }
    }
    fn skip_trivia(&mut self) {
        self.s.skip_trivia();
    }
    fn fresh_blank(&mut self) -> Term {
        let id = *self.blank_counter;
        *self.blank_counter += 1;
        Term::Blank(BlankNode::Anonymous(id))
    }
    fn emit(&mut self, t: TriplePattern) {
        self.triples.push(t);
    }

    /// A complete identifier-shaped word at the current position, or `None`
    /// if the byte here can't start one. Errors with `NeedMore` if the word
    /// might continue past the window's end — see `sparql::parser::Parser::peek_ident`.
    fn peek_ident(&self) -> PResult<Option<Span>> {
        match self.s.peek_word() {
            None if self.s.peek().is_none() && !self.s.eof => Err(DecodeError::NeedMore),
            None => Ok(None),
            Some(w) if self.s.word_needs_more(w) => Err(DecodeError::NeedMore),
            Some(w) => Ok(Some(w)),
        }
    }

    // ---- prefixed names / IRIs ----

    /// A confirmed `PN_PREFIX`-shaped `word` immediately followed by `:`.
    /// Resolves the prefix against the tokenizer's table *now* (rather than
    /// keeping it as a span), copying the namespace into a bounded inline
    /// [`Namespace`] — see [`Iri::Prefixed`].
    fn prefixed_name_from(&mut self, prefix: Span) -> PResult<Iri> {
        self.s.advance_to(prefix.end as usize);
        self.s.expect(b':')?;
        let local = if self.s.at_pn_local_start()? {
            self.s.pn_local()?
        } else {
            let p = self.s.pos as u32;
            Span::new(p, p)
        };
        let name = prefix.text(self.s.bytes);
        let Some((_, ns)) = self
            .prefixes
            .iter()
            .rev()
            .find(|(n, _)| n.as_slice() == name)
        else {
            return self.s.invalid("use of undeclared prefix");
        };
        let Some(namespace) = Namespace::new(ns) else {
            return self.s.invalid("prefix IRI too long");
        };
        Ok(Iri::Prefixed { namespace, local })
    }
    /// `iri ::= IRIREF | PrefixedName`.
    fn iri(&mut self) -> PResult<Iri> {
        self.skip_trivia();
        if self.s.peek() == Some(b'<') {
            return Ok(Iri::Full(self.s.iriref()?));
        }
        let Some(word) = self.peek_ident()? else {
            return self.s.invalid("expected IRI");
        };
        if self.s.bytes.get(word.end as usize) == Some(&b':') {
            self.prefixed_name_from(word)
        } else {
            self.s.invalid("expected IRI or prefixed name")
        }
    }
    fn rdf_literal(&mut self) -> PResult<Term> {
        let lexical = self.s.string_literal()?;
        let suffix = match self.s.peek() {
            None if !self.s.eof => return Err(DecodeError::NeedMore),
            Some(b'@') => LiteralSuffix::Lang(self.s.langtag()?),
            Some(b'^') => {
                self.s.pos += 1;
                self.s.expect(b'^')?;
                LiteralSuffix::Datatype(self.iri()?)
            }
            _ => LiteralSuffix::None,
        };
        Ok(Term::Literal(RdfLiteral { lexical, suffix }))
    }

    // ---- terms ----

    /// `subject ::= iri | BlankNode | collection` (`BlankNode` here meaning
    /// `BLANK_NODE_LABEL | ANON` — a bracketed property list as a top-level
    /// subject is handled separately by `triples_statement`, matching the
    /// grammar's own `triples` production).
    fn subject_term(&mut self) -> PResult<Term> {
        self.skip_trivia();
        match self.s.peek() {
            Some(b'(') => {
                if self.s.try_nil()? {
                    Ok(Term::Iri(Iri::WellKnown(WellKnown::RdfNil)))
                } else {
                    self.collection()
                }
            }
            Some(b'_') => Ok(Term::Blank(BlankNode::Labeled(self.s.blank_node_label()?))),
            _ => Ok(Term::Iri(self.iri()?)),
        }
    }
    /// `object ::= iri | BlankNode | collection | blankNodePropertyList | literal`.
    fn object_term(&mut self) -> PResult<Term> {
        self.skip_trivia();
        match self.s.peek() {
            Some(b'[') => self.bracketed_blank_node(),
            Some(b'(') => {
                if self.s.try_nil()? {
                    Ok(Term::Iri(Iri::WellKnown(WellKnown::RdfNil)))
                } else {
                    self.collection()
                }
            }
            Some(b'_') => Ok(Term::Blank(BlankNode::Labeled(self.s.blank_node_label()?))),
            Some(b'"' | b'\'') => self.rdf_literal(),
            Some(b) if b.is_ascii_digit() || matches!(b, b'.' | b'+' | b'-') => {
                match self.s.numeric_literal()? {
                    Some((kind, span)) => Ok(Term::Numeric(kind, span)),
                    None => self.s.invalid("expected numeric literal"),
                }
            }
            Some(b'<') => Ok(Term::Iri(self.iri()?)),
            _ => {
                let Some(word) = self.peek_ident()? else {
                    return self.s.invalid("expected object");
                };
                if self.s.bytes.get(word.end as usize) == Some(&b':') {
                    Ok(Term::Iri(self.prefixed_name_from(word)?))
                } else if word_eq(self.s.bytes, word, b"true") {
                    self.s.advance_to(word.end as usize);
                    Ok(Term::Boolean(true))
                } else if word_eq(self.s.bytes, word, b"false") {
                    self.s.advance_to(word.end as usize);
                    Ok(Term::Boolean(false))
                } else {
                    self.s.invalid("expected object")
                }
            }
        }
    }
    /// `verb ::= predicate | 'a'`. `'a'` is a bare, case-sensitive keyword —
    /// distinct from a prefixed name, so it's checked before falling back to
    /// `iri()`.
    fn verb(&mut self) -> PResult<Term> {
        self.skip_trivia();
        if self.s.peek() != Some(b'<')
            && let Some(word) = self.peek_ident()?
            && self.s.bytes.get(word.end as usize) != Some(&b':')
            && word_eq(self.s.bytes, word, b"a")
        {
            self.s.advance_to(word.end as usize);
            return Ok(Term::Iri(Iri::WellKnown(WellKnown::RdfType)));
        }
        Ok(Term::Iri(self.iri()?))
    }
    fn starts_verb(&self) -> bool {
        matches!(self.s.peek(), Some(b'<')) || self.s.peek_word().is_some()
    }

    // ---- lists ----

    fn object_list(&mut self, subject: Term, predicate: Term) -> PResult<()> {
        loop {
            self.skip_trivia();
            let object = self.object_term()?;
            self.emit(TriplePattern {
                subject,
                predicate,
                object,
            });
            self.skip_trivia();
            if self.s.peek() == Some(b',') {
                self.s.pos += 1;
            } else {
                return Ok(());
            }
        }
    }
    /// `predicateObjectList ::= verb objectList (';' (verb objectList)?)*`.
    /// The `verb objectList` after each `;` is itself optional, so `;;` and a
    /// trailing `;` right before the statement's `.` are both legal — a
    /// no-op semicolon, not an error — which is why this can't just stop the
    /// first time no verb follows a `;`: another `;` (with or without its
    /// own `verb objectList`) may still come after it.
    fn property_list_not_empty(&mut self, subject: Term) -> PResult<()> {
        self.skip_trivia();
        let predicate = self.verb()?;
        self.skip_trivia();
        self.object_list(subject, predicate)?;
        loop {
            self.skip_trivia();
            if self.s.peek() != Some(b';') {
                return Ok(());
            }
            self.s.pos += 1;
            self.skip_trivia();
            if !self.starts_verb() {
                continue;
            }
            let predicate = self.verb()?;
            self.skip_trivia();
            self.object_list(subject, predicate)?;
        }
    }
    /// `'[' predicateObjectList ']'`, or `ANON` (`[]`, no properties) —
    /// shared by both a top-level subject's `[...]` and a nested object's,
    /// which is why the *outer*, optional predicateObjectList that can
    /// follow a top-level `[...]` isn't part of this helper — see
    /// `triples_statement`.
    fn bracketed_blank_node(&mut self) -> PResult<Term> {
        if self.s.try_anon()? {
            return Ok(self.fresh_blank());
        }
        self.s.expect(b'[')?;
        let node = self.fresh_blank();
        self.skip_trivia();
        self.property_list_not_empty(node)?;
        self.skip_trivia();
        self.s.expect(b']')?;
        Ok(node)
    }
    /// `collection ::= '(' object* ')'`. A syntactic empty collection (no
    /// elements, whether or not `try_nil` matched it — a comment between the
    /// parens still leaves zero objects) is `rdf:nil` with no triples
    /// emitted; a non-empty one becomes an `rdf:first`/`rdf:rest` list.
    fn collection(&mut self) -> PResult<Term> {
        self.s.expect(b'(')?;
        self.skip_trivia();
        if self.s.peek() == Some(b')') {
            self.s.pos += 1;
            return Ok(Term::Iri(Iri::WellKnown(WellKnown::RdfNil)));
        }
        let head = self.fresh_blank();
        let mut cur = head;
        loop {
            let el = self.object_term()?;
            self.skip_trivia();
            let is_last = self.s.peek() == Some(b')');
            let rest = if is_last {
                Term::Iri(Iri::WellKnown(WellKnown::RdfNil))
            } else {
                self.fresh_blank()
            };
            self.emit(TriplePattern {
                subject: cur,
                predicate: Term::Iri(Iri::WellKnown(WellKnown::RdfFirst)),
                object: el,
            });
            self.emit(TriplePattern {
                subject: cur,
                predicate: Term::Iri(Iri::WellKnown(WellKnown::RdfRest)),
                object: rest,
            });
            if is_last {
                self.s.pos += 1;
                break;
            }
            cur = rest;
            self.skip_trivia();
        }
        Ok(head)
    }

    // ---- directives ----

    fn try_exact_keyword(&mut self, kw: &[u8]) -> PResult<bool> {
        match self.peek_ident()? {
            Some(w) if word_eq(self.s.bytes, w, kw) => {
                self.s.advance_to(w.end as usize);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
    /// `PNAME_NS IRIREF` shared by `@prefix` and SPARQL-style `PREFIX`,
    /// declaring (or redeclaring) one entry in the tokenizer's persistent
    /// prefix table.
    fn prefix_decl(&mut self) -> PResult<()> {
        self.skip_trivia();
        let name_span = if self.s.peek() == Some(b':') {
            let p = self.s.pos as u32;
            Span::new(p, p)
        } else {
            let Some(word) = self.peek_ident()? else {
                return self.s.invalid("expected prefix name");
            };
            self.s.advance_to(word.end as usize);
            word
        };
        self.s.expect(b':')?;
        self.skip_trivia();
        let ns_span = self.s.iriref()?;
        let name = name_span.text(self.s.bytes).to_vec();
        let ns = ns_span.text(self.s.bytes).to_vec();
        match self.prefixes.iter_mut().find(|(n, _)| *n == name) {
            Some(entry) => entry.1 = ns,
            None => self.prefixes.push((name, ns)),
        }
        Ok(())
    }
    /// Tries to parse one directive (`@prefix`/`@base`/`PREFIX`/`BASE`).
    /// Returns `false`, having consumed nothing, if the input doesn't start
    /// with one — the caller then parses a triples statement instead.
    /// `BASE`'s IRI is parsed for well-formedness only and discarded:
    /// relative-IRI resolution is left to the caller, the same lexical-layer
    /// stance `sparql::term::Prologue::resolve` documents for SPARQL's `BASE`.
    fn try_directive(&mut self) -> PResult<bool> {
        self.skip_trivia();
        if self.s.peek() == Some(b'@') {
            self.s.pos += 1;
            if self.try_exact_keyword(b"prefix")? {
                self.skip_trivia();
                self.prefix_decl()?;
                self.skip_trivia();
                self.s.expect(b'.')?;
                return Ok(true);
            }
            if self.try_exact_keyword(b"base")? {
                self.skip_trivia();
                let _ = self.s.iriref()?;
                self.skip_trivia();
                self.s.expect(b'.')?;
                return Ok(true);
            }
            return self.s.invalid("expected 'prefix' or 'base' after '@'");
        }
        if let Some(word) = self.peek_ident()? {
            if word_eq_ci(self.s.bytes, word, b"PREFIX") {
                self.s.advance_to(word.end as usize);
                self.skip_trivia();
                self.prefix_decl()?;
                return Ok(true);
            }
            if word_eq_ci(self.s.bytes, word, b"BASE") {
                self.s.advance_to(word.end as usize);
                self.skip_trivia();
                let _ = self.s.iriref()?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    // ---- statement ----

    /// `triples ::= subject predicateObjectList | blankNodePropertyList predicateObjectList?`.
    fn triples_statement(&mut self) -> PResult<()> {
        self.skip_trivia();
        if self.s.peek() == Some(b'[') {
            let node = self.bracketed_blank_node()?;
            self.skip_trivia();
            if self.starts_verb() {
                self.property_list_not_empty(node)?;
            }
        } else {
            let subject = self.subject_term()?;
            self.skip_trivia();
            self.property_list_not_empty(subject)?;
        }
        self.skip_trivia();
        self.s.expect(b'.')?;
        Ok(())
    }

    /// Parses one statement (directive or triples). `Ok((None, consumed))`
    /// means a directive updated tokenizer state but produced no triples —
    /// the same "nothing to yield, but forward progress was made" shape
    /// `nt_tokenizer` uses for blank lines and comments.
    pub fn parse(mut self) -> PResult<(Option<StatementData>, usize)> {
        self.skip_trivia();
        if self.try_directive()? {
            return Ok((None, self.s.pos));
        }
        self.triples_statement()?;
        Ok((Some(self.triples), self.s.pos))
    }
}

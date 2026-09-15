//! Recursive-descent SPARQL 1.1 Query parser, translating directly into the
//! [`super::algebra`] tree rather than an intermediate concrete-syntax AST.
//! Driven by [`super::lexer::Scanner`] the same way `nt_tokenizer` drives its
//! own `Scanner`: which grammar alternative a byte starts depends on parser
//! state, so lexing and parsing stay merged.
use super::algebra::{Algebra, OrderKey, Projection, QueryData, QueryForm, TriplePattern};
use super::expr::{AggKind, Aggregate, Builtin, Expr, GroupAggregate};
use super::ids::{ExprId, NodeId, PathId};
use super::lexer::Scanner;
use super::path::{NegPathItem, PathExpr};
use super::pool::Range;
use super::term::{BlankNode, Iri, LiteralSuffix, RdfLiteral, Span, Term, Var, WellKnown};
use crate::tokenizer::DecodeError;

pub(super) type PResult<T> = std::result::Result<T, DecodeError>;

fn word_eq_ci(bytes: &[u8], span: Span, target: &[u8]) -> bool {
    span.text(bytes).eq_ignore_ascii_case(target)
}
fn word_eq(bytes: &[u8], span: Span, target: &[u8]) -> bool {
    span.text(bytes) == target
}

/// One triple produced while expanding a `TriplesBlock`: either a plain
/// triple (destined for a `Bgp`) or one whose predicate is a genuine
/// property path (destined for its own `Path` node). Kept apart so runs of
/// plain triples can be merged into a single `Bgp`.
enum RawTriple {
    Simple(TriplePattern),
    Complex(Term, PathId, Term),
}

enum Verb {
    Simple(Term),
    Path(PathId),
}

/// One `GraphPatternNotTriples` alternative's contribution to its enclosing
/// `GroupGraphPatternSub`. `Optional`/`Minus` are kept apart from `Pattern`
/// because they combine with the accumulated left side through a dedicated
/// binary operator, not a plain `Join` — see [`Algebra::LeftJoin`].
enum NotTriples {
    Pattern(NodeId),
    Optional(NodeId, Option<ExprId>),
    Minus(NodeId),
    Filter(ExprId),
    Bind(Var, ExprId),
}

/// Per-`SELECT`/`SubSelect` state threaded through solution-modifier
/// parsing so aggregate hoisting (see [`Expr::Aggregate`]) can see every
/// expression that might contain one: the projection list, `HAVING`, and
/// `ORDER BY` all parse before hoisting runs once, after `GROUP BY`.
enum ProjectionSpec {
    All,
    Items(Vec<ProjectionItem>),
}
enum ProjectionItem {
    Var(Var),
    Expr(ExprId, Var),
}

pub(super) struct Parser<'a> {
    s: Scanner<'a>,
    q: QueryData,
    blank_counter: u32,
    synth_counter: u32,
}

impl<'a> Parser<'a> {
    pub fn new(bytes: &'a [u8], eof: bool) -> Self {
        Self {
            s: Scanner::new(bytes, eof),
            q: QueryData::default(),
            blank_counter: 0,
            synth_counter: 0,
        }
    }
    fn skip_trivia(&mut self) {
        self.s.skip_trivia();
    }
    fn push_node(&mut self, a: Algebra) -> NodeId {
        self.q.nodes.push(a)
    }
    fn push_expr(&mut self, e: Expr) -> ExprId {
        self.q.exprs.push(e)
    }
    fn push_path(&mut self, p: PathExpr) -> PathId {
        self.q.paths.push(p)
    }
    fn unit(&mut self) -> NodeId {
        self.push_node(Algebra::Unit)
    }
    fn join(&mut self, acc: Option<NodeId>, next: NodeId) -> NodeId {
        match acc {
            None => next,
            Some(prev) => self.push_node(Algebra::Join(prev, next)),
        }
    }
    fn fresh_blank(&mut self) -> Term {
        let id = self.blank_counter;
        self.blank_counter += 1;
        Term::Blank(BlankNode::Anonymous(id))
    }
    fn fresh_var(&mut self) -> Var {
        let id = self.synth_counter;
        self.synth_counter += 1;
        Var::Synthetic(id)
    }

    // ---- terms ----

    fn var(&mut self) -> PResult<Var> {
        Ok(Var::Named(self.s.var()?))
    }
    /// A confirmed `PN_PREFIX`-shaped `word` immediately followed by `:`.
    fn prefixed_name_from(&mut self, prefix: Span) -> PResult<Iri> {
        self.s.advance_to(prefix.end as usize);
        self.s.expect(b':')?;
        if self.s.at_pn_local_start()? {
            let local = self.s.pn_local()?;
            Ok(Iri::Prefixed { prefix, local })
        } else {
            let empty = self.s.pos as u32;
            Ok(Iri::Prefixed {
                prefix,
                local: Span::new(empty, empty),
            })
        }
    }
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
    fn graph_term(&mut self) -> PResult<Term> {
        self.skip_trivia();
        match self.s.peek() {
            Some(b'<') => Ok(Term::Iri(self.iri()?)),
            Some(b'"' | b'\'') => self.rdf_literal(),
            Some(b'_') => Ok(Term::Blank(BlankNode::Labeled(self.s.blank_node_label()?))),
            Some(b'(') => {
                if self.s.try_nil()? {
                    Ok(Term::Iri(Iri::WellKnown(WellKnown::RdfNil)))
                } else {
                    self.s.invalid("expected NIL")
                }
            }
            Some(b) if b.is_ascii_digit() || matches!(b, b'.' | b'+' | b'-') => {
                match self.s.numeric_literal()? {
                    Some((kind, span)) => Ok(Term::Numeric(kind, span)),
                    None => self.s.invalid("expected numeric literal"),
                }
            }
            _ => {
                let Some(word) = self.peek_ident()? else {
                    return self.s.invalid("expected term");
                };
                if self.s.bytes.get(word.end as usize) == Some(&b':') {
                    Ok(Term::Iri(self.prefixed_name_from(word)?))
                } else if word_eq_ci(self.s.bytes, word, b"true") {
                    self.s.advance_to(word.end as usize);
                    Ok(Term::Boolean(true))
                } else if word_eq_ci(self.s.bytes, word, b"false") {
                    self.s.advance_to(word.end as usize);
                    Ok(Term::Boolean(false))
                } else {
                    self.s.invalid("expected term")
                }
            }
        }
    }
    fn var_or_term(&mut self) -> PResult<Term> {
        self.skip_trivia();
        if matches!(self.s.peek(), Some(b'?' | b'$')) {
            Ok(Term::Var(self.var()?))
        } else {
            self.graph_term()
        }
    }
    fn var_or_term_or_triples_node(&mut self, items: &mut Vec<RawTriple>) -> PResult<Term> {
        self.skip_trivia();
        match self.s.peek() {
            Some(b'[') => self.blank_node_property_list(items),
            Some(b'(') => {
                if self.s.try_nil()? {
                    Ok(Term::Iri(Iri::WellKnown(WellKnown::RdfNil)))
                } else {
                    self.collection(items)
                }
            }
            _ => self.var_or_term(),
        }
    }

    // ---- property paths ----

    fn path(&mut self) -> PResult<PathId> {
        self.path_alternative()
    }
    fn path_alternative(&mut self) -> PResult<PathId> {
        let mut left = self.path_sequence()?;
        loop {
            self.skip_trivia();
            if self.s.peek() == Some(b'|') {
                self.s.pos += 1;
                let right = self.path_sequence()?;
                left = self.push_path(PathExpr::Alt(left, right));
            } else {
                return Ok(left);
            }
        }
    }
    fn path_sequence(&mut self) -> PResult<PathId> {
        let mut left = self.path_elt_or_inverse()?;
        loop {
            self.skip_trivia();
            if self.s.peek() == Some(b'/') {
                self.s.pos += 1;
                let right = self.path_elt_or_inverse()?;
                left = self.push_path(PathExpr::Seq(left, right));
            } else {
                return Ok(left);
            }
        }
    }
    fn path_elt_or_inverse(&mut self) -> PResult<PathId> {
        self.skip_trivia();
        if self.s.peek() == Some(b'^') {
            self.s.pos += 1;
            let inner = self.path_elt()?;
            Ok(self.push_path(PathExpr::Inv(inner)))
        } else {
            self.path_elt()
        }
    }
    fn path_elt(&mut self) -> PResult<PathId> {
        let primary = self.path_primary()?;
        self.path_mod(primary)
    }
    /// Maximal munch always lexes `?` immediately followed by a `VARNAME`
    /// start as one `Var` token, never as a bare `?`: `<urn:p> ?o` is a path
    /// with no modifier followed by the object `?o`, not `ZeroOrOne` eating
    /// the object's sigil. Only a `?` with no such follower can be `PathMod`.
    fn looks_like_var_start_at(&self, offset: usize) -> bool {
        matches!(self.s.peek_at(offset), Some(b) if b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80)
    }
    fn path_mod(&mut self, inner: PathId) -> PResult<PathId> {
        self.skip_trivia();
        match self.s.peek() {
            Some(b'*') => {
                self.s.pos += 1;
                Ok(self.push_path(PathExpr::ZeroOrMore(inner)))
            }
            Some(b'+') => {
                self.s.pos += 1;
                Ok(self.push_path(PathExpr::OneOrMore(inner)))
            }
            Some(b'?') if !self.looks_like_var_start_at(1) => {
                self.s.pos += 1;
                Ok(self.push_path(PathExpr::ZeroOrOne(inner)))
            }
            _ => Ok(inner),
        }
    }
    fn path_primary(&mut self) -> PResult<PathId> {
        self.skip_trivia();
        match self.s.peek() {
            Some(b'(') => {
                self.s.expect(b'(')?;
                let inner = self.path()?;
                self.skip_trivia();
                self.s.expect(b')')?;
                Ok(inner)
            }
            Some(b'!') => {
                self.s.pos += 1;
                self.skip_trivia();
                self.path_negated_property_set()
            }
            Some(b'<') => {
                let span = self.s.iriref()?;
                Ok(self.push_path(PathExpr::Link(Iri::Full(span))))
            }
            _ => {
                let Some(word) = self.peek_ident()? else {
                    return self.s.invalid("expected path primary");
                };
                if self.s.bytes.get(word.end as usize) == Some(&b':') {
                    let iri = self.prefixed_name_from(word)?;
                    Ok(self.push_path(PathExpr::Link(iri)))
                } else if word_eq(self.s.bytes, word, b"a") {
                    self.s.advance_to(word.end as usize);
                    Ok(self.push_path(PathExpr::Link(Iri::WellKnown(WellKnown::RdfType))))
                } else {
                    self.s.invalid("expected IRI, 'a', or prefixed name")
                }
            }
        }
    }
    fn path_one_in_property_set(&mut self) -> PResult<NegPathItem> {
        self.skip_trivia();
        let inverse = if self.s.peek() == Some(b'^') {
            self.s.pos += 1;
            true
        } else {
            false
        };
        let iri = if self.s.peek() == Some(b'<') {
            Iri::Full(self.s.iriref()?)
        } else {
            let Some(word) = self.peek_ident()? else {
                return self.s.invalid("expected IRI in negated property set");
            };
            if self.s.bytes.get(word.end as usize) == Some(&b':') {
                self.prefixed_name_from(word)?
            } else if word_eq(self.s.bytes, word, b"a") {
                self.s.advance_to(word.end as usize);
                Iri::WellKnown(WellKnown::RdfType)
            } else {
                return self.s.invalid("expected IRI, 'a', or prefixed name");
            }
        };
        Ok(NegPathItem { inverse, iri })
    }
    fn path_negated_property_set(&mut self) -> PResult<PathId> {
        let start = self.q.neg_path_items.start();
        if self.s.peek() == Some(b'(') {
            self.s.pos += 1;
            self.skip_trivia();
            if self.s.peek() != Some(b')') {
                loop {
                    let item = self.path_one_in_property_set()?;
                    self.q.neg_path_items.push(item);
                    self.skip_trivia();
                    if self.s.peek() == Some(b'|') {
                        self.s.pos += 1;
                        self.skip_trivia();
                    } else {
                        break;
                    }
                }
            }
            self.skip_trivia();
            self.s.expect(b')')?;
        } else {
            let item = self.path_one_in_property_set()?;
            self.q.neg_path_items.push(item);
        }
        let range = self.q.neg_path_items.range_from(start);
        Ok(self.push_path(PathExpr::NegatedPropertySet(range)))
    }

    // ---- triples blocks ----

    fn starts_triples(&self) -> bool {
        !matches!(self.s.peek(), None | Some(b'}'))
    }
    fn verb(&mut self) -> PResult<Verb> {
        self.skip_trivia();
        if matches!(self.s.peek(), Some(b'?' | b'$')) {
            return Ok(Verb::Simple(Term::Var(self.var()?)));
        }
        let path = self.path()?;
        if let PathExpr::Link(iri) = self.q.path(path) {
            Ok(Verb::Simple(Term::Iri(iri)))
        } else {
            Ok(Verb::Path(path))
        }
    }
    fn object_list(
        &mut self,
        subject: Term,
        verb: &Verb,
        items: &mut Vec<RawTriple>,
    ) -> PResult<()> {
        loop {
            self.skip_trivia();
            let object = self.var_or_term_or_triples_node(items)?;
            items.push(match *verb {
                Verb::Simple(predicate) => RawTriple::Simple(TriplePattern {
                    subject,
                    predicate,
                    object,
                }),
                Verb::Path(path) => RawTriple::Complex(subject, path, object),
            });
            self.skip_trivia();
            if self.s.peek() == Some(b',') {
                self.s.pos += 1;
            } else {
                return Ok(());
            }
        }
    }
    fn starts_verb(&self) -> bool {
        matches!(self.s.peek(), Some(b'?' | b'$' | b'<' | b'!' | b'^' | b'('))
            || self.s.peek_word().is_some()
    }
    fn property_list_not_empty(
        &mut self,
        subject: Term,
        items: &mut Vec<RawTriple>,
    ) -> PResult<()> {
        loop {
            self.skip_trivia();
            let verb = self.verb()?;
            self.skip_trivia();
            self.object_list(subject, &verb, items)?;
            self.skip_trivia();
            if self.s.peek() == Some(b';') {
                self.s.pos += 1;
                self.skip_trivia();
                if !self.starts_verb() {
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        }
    }
    fn collection(&mut self, items: &mut Vec<RawTriple>) -> PResult<Term> {
        self.s.expect(b'(')?;
        self.skip_trivia();
        let mut elements = Vec::new();
        loop {
            elements.push(self.var_or_term_or_triples_node(items)?);
            self.skip_trivia();
            if self.s.peek() == Some(b')') {
                break;
            }
        }
        self.s.expect(b')')?;
        let head = self.fresh_blank();
        let mut cur = head;
        let n = elements.len();
        for (i, el) in elements.into_iter().enumerate() {
            items.push(RawTriple::Simple(TriplePattern {
                subject: cur,
                predicate: Term::Iri(Iri::WellKnown(WellKnown::RdfFirst)),
                object: el,
            }));
            let rest = if i + 1 < n {
                self.fresh_blank()
            } else {
                Term::Iri(Iri::WellKnown(WellKnown::RdfNil))
            };
            items.push(RawTriple::Simple(TriplePattern {
                subject: cur,
                predicate: Term::Iri(Iri::WellKnown(WellKnown::RdfRest)),
                object: rest,
            }));
            cur = rest;
        }
        Ok(head)
    }
    fn blank_node_property_list(&mut self, items: &mut Vec<RawTriple>) -> PResult<Term> {
        if self.s.try_anon()? {
            return Ok(self.fresh_blank());
        }
        self.s.expect(b'[')?;
        let node = self.fresh_blank();
        self.skip_trivia();
        self.property_list_not_empty(node, items)?;
        self.skip_trivia();
        self.s.expect(b']')?;
        Ok(node)
    }
    fn triples_same_subject_path(&mut self, items: &mut Vec<RawTriple>) -> PResult<()> {
        self.skip_trivia();
        let subject = self.var_or_term_or_triples_node(items)?;
        self.skip_trivia();
        self.property_list_not_empty(subject, items)?;
        Ok(())
    }
    fn build_triples_node(&mut self, items: Vec<RawTriple>) -> NodeId {
        let mut acc: Option<NodeId> = None;
        let mut pending: Vec<TriplePattern> = Vec::new();
        for item in items {
            match item {
                RawTriple::Simple(t) => pending.push(t),
                RawTriple::Complex(s, p, o) => {
                    if let Some(node) = self.flush_pending(&mut pending) {
                        acc = Some(self.join(acc, node));
                    }
                    let node = self.push_node(Algebra::Path {
                        subject: s,
                        path: p,
                        object: o,
                    });
                    acc = Some(self.join(acc, node));
                }
            }
        }
        if let Some(node) = self.flush_pending(&mut pending) {
            acc = Some(self.join(acc, node));
        }
        acc.unwrap_or_else(|| self.unit())
    }
    fn flush_pending(&mut self, pending: &mut Vec<TriplePattern>) -> Option<NodeId> {
        if pending.is_empty() {
            return None;
        }
        let start = self.q.triples.start();
        for t in pending.drain(..) {
            self.q.triples.push(t);
        }
        let range = self.q.triples.range_from(start);
        Some(self.push_node(Algebra::Bgp(range)))
    }
    fn triples_block(&mut self) -> PResult<NodeId> {
        let mut items = Vec::new();
        self.triples_same_subject_path(&mut items)?;
        loop {
            self.skip_trivia();
            if self.s.peek() != Some(b'.') {
                break;
            }
            self.s.pos += 1;
            self.skip_trivia();
            if !self.starts_triples() || self.is_graph_pattern_not_triples_keyword() {
                break;
            }
            self.triples_same_subject_path(&mut items)?;
        }
        Ok(self.build_triples_node(items))
    }
}

impl<'a> Parser<'a> {
    // ---- keyword helpers ----

    /// A complete identifier-shaped word at the current position, or `None`
    /// if the byte here can't start one. Errors with `NeedMore` if the word
    /// might continue past the window's end.
    fn peek_ident(&self) -> PResult<Option<Span>> {
        match self.s.peek_word() {
            None if self.s.peek().is_none() && !self.s.eof => Err(DecodeError::NeedMore),
            None => Ok(None),
            Some(w) if self.s.word_needs_more(w) => Err(DecodeError::NeedMore),
            Some(w) => Ok(Some(w)),
        }
    }
    fn try_keyword(&mut self, kw: &[u8]) -> PResult<bool> {
        match self.peek_ident()? {
            Some(w) if word_eq_ci(self.s.bytes, w, kw) => {
                self.s.advance_to(w.end as usize);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
    fn expect_keyword(&mut self, kw: &[u8]) -> PResult<()> {
        self.skip_trivia();
        if self.try_keyword(kw)? {
            Ok(())
        } else {
            self.s.invalid("expected keyword")
        }
    }
    fn conjoin(&mut self, exprs: Vec<ExprId>) -> Option<ExprId> {
        let mut it = exprs.into_iter();
        let mut acc = it.next()?;
        for e in it {
            acc = self.push_expr(Expr::And(acc, e));
        }
        Some(acc)
    }

    // ---- group graph patterns ----

    fn looks_like_not_triples(&self) -> PResult<bool> {
        if self.s.peek() == Some(b'{') {
            return Ok(true);
        }
        let Some(w) = self.peek_ident()? else {
            return Ok(false);
        };
        for kw in [
            b"OPTIONAL".as_slice(),
            b"MINUS",
            b"GRAPH",
            b"FILTER",
            b"BIND",
            b"VALUES",
        ] {
            if word_eq_ci(self.s.bytes, w, kw) {
                return Ok(true);
            }
        }
        if word_eq_ci(self.s.bytes, w, b"SERVICE") {
            return self.s.invalid("SERVICE (federated query) is not supported");
        }
        Ok(false)
    }
    fn is_graph_pattern_not_triples_keyword(&self) -> bool {
        self.looks_like_not_triples().unwrap_or(false)
    }

    fn group_graph_pattern_sub(&mut self) -> PResult<(NodeId, Vec<ExprId>)> {
        let mut acc: Option<NodeId> = None;
        let mut filters: Vec<ExprId> = Vec::new();
        loop {
            self.skip_trivia();
            match self.s.peek() {
                None => return self.s.missing(),
                Some(b'}') => break,
                _ => {
                    if self.looks_like_not_triples()? {
                        match self.graph_pattern_not_triples()? {
                            NotTriples::Pattern(node) => acc = Some(self.join(acc, node)),
                            NotTriples::Optional(inner, cond) => {
                                let base = acc.unwrap_or_else(|| self.unit());
                                acc = Some(self.push_node(Algebra::LeftJoin(base, inner, cond)));
                            }
                            NotTriples::Minus(inner) => {
                                let base = acc.unwrap_or_else(|| self.unit());
                                acc = Some(self.push_node(Algebra::Minus(base, inner)));
                            }
                            NotTriples::Filter(e) => filters.push(e),
                            NotTriples::Bind(var, e) => {
                                let base = acc.unwrap_or_else(|| self.unit());
                                acc = Some(self.push_node(Algebra::Extend(base, var, e)));
                            }
                        }
                        self.skip_trivia();
                        if self.s.peek() == Some(b'.') {
                            self.s.pos += 1;
                        }
                    } else {
                        let node = self.triples_block()?;
                        acc = Some(self.join(acc, node));
                    }
                }
            }
        }
        Ok((acc.unwrap_or_else(|| self.unit()), filters))
    }
    fn wrap_filters(&mut self, base: NodeId, filters: Vec<ExprId>) -> NodeId {
        match self.conjoin(filters) {
            Some(e) => self.push_node(Algebra::Filter(e, base)),
            None => base,
        }
    }
    fn peek_starts_subselect(&self) -> PResult<bool> {
        match self.peek_ident()? {
            Some(w) => Ok(word_eq_ci(self.s.bytes, w, b"SELECT")),
            None => Ok(false),
        }
    }
    fn group_graph_pattern(&mut self) -> PResult<NodeId> {
        self.skip_trivia();
        self.s.expect(b'{')?;
        self.skip_trivia();
        let node = if self.peek_starts_subselect()? {
            self.sub_select()?
        } else {
            let (base, filters) = self.group_graph_pattern_sub()?;
            self.wrap_filters(base, filters)
        };
        self.skip_trivia();
        self.s.expect(b'}')?;
        Ok(node)
    }
    fn group_or_union_graph_pattern(&mut self) -> PResult<NodeId> {
        let mut left = self.group_graph_pattern()?;
        loop {
            self.skip_trivia();
            if self.try_keyword(b"UNION")? {
                self.skip_trivia();
                let right = self.group_graph_pattern()?;
                left = self.push_node(Algebra::Union(left, right));
            } else {
                return Ok(left);
            }
        }
    }
    fn graph_pattern_not_triples(&mut self) -> PResult<NotTriples> {
        self.skip_trivia();
        if self.s.peek() == Some(b'{') {
            return Ok(NotTriples::Pattern(self.group_or_union_graph_pattern()?));
        }
        if self.try_keyword(b"OPTIONAL")? {
            self.skip_trivia();
            self.s.expect(b'{')?;
            self.skip_trivia();
            let (inner, filters) = if self.peek_starts_subselect()? {
                (self.sub_select()?, Vec::new())
            } else {
                self.group_graph_pattern_sub()?
            };
            self.skip_trivia();
            self.s.expect(b'}')?;
            return Ok(NotTriples::Optional(inner, self.conjoin(filters)));
        }
        if self.try_keyword(b"MINUS")? {
            let inner = self.group_graph_pattern()?;
            return Ok(NotTriples::Minus(inner));
        }
        if self.try_keyword(b"GRAPH")? {
            self.skip_trivia();
            let term = self.var_or_term()?;
            let inner = self.group_graph_pattern()?;
            return Ok(NotTriples::Pattern(
                self.push_node(Algebra::Graph(term, inner)),
            ));
        }
        if self.try_keyword(b"FILTER")? {
            self.skip_trivia();
            return Ok(NotTriples::Filter(self.constraint()?));
        }
        if self.try_keyword(b"BIND")? {
            self.skip_trivia();
            self.s.expect(b'(')?;
            self.skip_trivia();
            let expr = self.expression()?;
            self.skip_trivia();
            self.expect_keyword(b"AS")?;
            self.skip_trivia();
            let var = self.var()?;
            self.skip_trivia();
            self.s.expect(b')')?;
            return Ok(NotTriples::Bind(var, expr));
        }
        if self.try_keyword(b"VALUES")? {
            return Ok(NotTriples::Pattern(self.inline_data()?));
        }
        self.s.invalid("expected graph pattern")
    }

    // ---- VALUES / inline data ----

    fn data_block_value(&mut self) -> PResult<Option<Term>> {
        self.skip_trivia();
        if self.try_keyword(b"UNDEF")? {
            return Ok(None);
        }
        Ok(Some(self.graph_term()?))
    }
    fn inline_data(&mut self) -> PResult<NodeId> {
        self.skip_trivia();
        let vars_start = self.q.vars.start();
        if matches!(self.s.peek(), Some(b'?' | b'$')) {
            let v = self.var()?;
            self.q.vars.push(v);
            self.skip_trivia();
            self.s.expect(b'{')?;
            let rows_start = self.q.values.start();
            loop {
                self.skip_trivia();
                if self.s.peek() == Some(b'}') {
                    break;
                }
                let value = self.data_block_value()?;
                self.q.values.push(value);
            }
            self.s.expect(b'}')?;
            let vars = self.q.vars.range_from(vars_start);
            let rows = self.q.values.range_from(rows_start);
            return Ok(self.push_node(Algebra::Table { vars, rows }));
        }
        self.s.expect(b'(')?;
        loop {
            self.skip_trivia();
            if self.s.peek() == Some(b')') {
                break;
            }
            let v = self.var()?;
            self.q.vars.push(v);
            self.skip_trivia();
        }
        self.s.expect(b')')?;
        let vars = self.q.vars.range_from(vars_start);
        let nvars = vars.len as usize;
        self.skip_trivia();
        self.s.expect(b'{')?;
        let rows_start = self.q.values.start();
        loop {
            self.skip_trivia();
            if self.s.peek() == Some(b'}') {
                break;
            }
            self.skip_trivia();
            if self.s.try_nil()? {
                if nvars != 0 {
                    return self
                        .s
                        .invalid("VALUES row arity does not match variable list");
                }
                continue;
            }
            self.s.expect(b'(')?;
            let mut count = 0;
            loop {
                self.skip_trivia();
                if self.s.peek() == Some(b')') {
                    break;
                }
                let v = self.data_block_value()?;
                self.q.values.push(v);
                count += 1;
            }
            self.s.expect(b')')?;
            if count != nvars {
                return self
                    .s
                    .invalid("VALUES row arity does not match variable list");
            }
        }
        self.s.expect(b'}')?;
        let rows = self.q.values.range_from(rows_start);
        Ok(self.push_node(Algebra::Table { vars, rows }))
    }
}

impl<'a> Parser<'a> {
    // ---- expressions ----

    pub(super) fn expression(&mut self) -> PResult<ExprId> {
        self.conditional_or_expression()
    }
    fn conditional_or_expression(&mut self) -> PResult<ExprId> {
        let mut left = self.conditional_and_expression()?;
        loop {
            self.skip_trivia();
            if self.s.peek() == Some(b'|') && self.s.peek_at(1) == Some(b'|') {
                self.s.pos += 2;
                let right = self.conditional_and_expression()?;
                left = self.push_expr(Expr::Or(left, right));
            } else {
                return Ok(left);
            }
        }
    }
    fn conditional_and_expression(&mut self) -> PResult<ExprId> {
        let mut left = self.relational_expression()?;
        loop {
            self.skip_trivia();
            if self.s.peek() == Some(b'&') && self.s.peek_at(1) == Some(b'&') {
                self.s.pos += 2;
                let right = self.relational_expression()?;
                left = self.push_expr(Expr::And(left, right));
            } else {
                return Ok(left);
            }
        }
    }
    fn relational_expression(&mut self) -> PResult<ExprId> {
        let left = self.additive_expression()?;
        self.skip_trivia();
        match self.s.peek() {
            Some(b'=') => {
                self.s.pos += 1;
                let r = self.additive_expression()?;
                Ok(self.push_expr(Expr::Eq(left, r)))
            }
            Some(b'!') if self.s.peek_at(1) == Some(b'=') => {
                self.s.pos += 2;
                let r = self.additive_expression()?;
                Ok(self.push_expr(Expr::Ne(left, r)))
            }
            Some(b'<') if self.s.peek_at(1) == Some(b'=') => {
                self.s.pos += 2;
                let r = self.additive_expression()?;
                Ok(self.push_expr(Expr::Le(left, r)))
            }
            Some(b'<') => {
                self.s.pos += 1;
                let r = self.additive_expression()?;
                Ok(self.push_expr(Expr::Lt(left, r)))
            }
            Some(b'>') if self.s.peek_at(1) == Some(b'=') => {
                self.s.pos += 2;
                let r = self.additive_expression()?;
                Ok(self.push_expr(Expr::Ge(left, r)))
            }
            Some(b'>') => {
                self.s.pos += 1;
                let r = self.additive_expression()?;
                Ok(self.push_expr(Expr::Gt(left, r)))
            }
            _ => {
                if self.try_keyword(b"NOT")? {
                    self.skip_trivia();
                    self.expect_keyword(b"IN")?;
                    let list = self.expression_list()?;
                    Ok(self.push_expr(Expr::NotIn(left, list)))
                } else if self.try_keyword(b"IN")? {
                    let list = self.expression_list()?;
                    Ok(self.push_expr(Expr::In(left, list)))
                } else {
                    Ok(left)
                }
            }
        }
    }
    fn expression_list(&mut self) -> PResult<Range> {
        self.skip_trivia();
        self.s.expect(b'(')?;
        self.skip_trivia();
        let start = self.q.expr_lists.start();
        if self.s.peek() != Some(b')') {
            loop {
                let e = self.expression()?;
                self.q.expr_lists.push(e);
                self.skip_trivia();
                if self.s.peek() == Some(b',') {
                    self.s.pos += 1;
                    self.skip_trivia();
                } else {
                    break;
                }
            }
        }
        self.s.expect(b')')?;
        Ok(self.q.expr_lists.range_from(start))
    }
    fn is_signed_numeric_start(&self) -> bool {
        matches!(self.s.peek_at(1), Some(b) if b.is_ascii_digit() || b == b'.')
    }
    fn additive_expression(&mut self) -> PResult<ExprId> {
        let mut left = self.multiplicative_expression()?;
        loop {
            self.skip_trivia();
            match self.s.peek() {
                Some(b'+') if !self.is_signed_numeric_start() => {
                    self.s.pos += 1;
                    let r = self.multiplicative_expression()?;
                    left = self.push_expr(Expr::Add(left, r));
                }
                Some(b'-') if !self.is_signed_numeric_start() => {
                    self.s.pos += 1;
                    let r = self.multiplicative_expression()?;
                    left = self.push_expr(Expr::Sub(left, r));
                }
                Some(b'+' | b'-') => {
                    let (kind, span) = self
                        .s
                        .numeric_literal()?
                        .expect("sign immediately followed by a digit or '.'");
                    let mut term = self.push_expr(Expr::Term(Term::Numeric(kind, span)));
                    loop {
                        self.skip_trivia();
                        match self.s.peek() {
                            Some(b'*') => {
                                self.s.pos += 1;
                                let u = self.unary_expression()?;
                                term = self.push_expr(Expr::Mul(term, u));
                            }
                            Some(b'/') => {
                                self.s.pos += 1;
                                let u = self.unary_expression()?;
                                term = self.push_expr(Expr::Div(term, u));
                            }
                            _ => break,
                        }
                    }
                    left = self.push_expr(Expr::Add(left, term));
                }
                _ => return Ok(left),
            }
        }
    }
    fn multiplicative_expression(&mut self) -> PResult<ExprId> {
        let mut left = self.unary_expression()?;
        loop {
            self.skip_trivia();
            match self.s.peek() {
                Some(b'*') => {
                    self.s.pos += 1;
                    let r = self.unary_expression()?;
                    left = self.push_expr(Expr::Mul(left, r));
                }
                Some(b'/') => {
                    self.s.pos += 1;
                    let r = self.unary_expression()?;
                    left = self.push_expr(Expr::Div(left, r));
                }
                _ => return Ok(left),
            }
        }
    }
    fn unary_expression(&mut self) -> PResult<ExprId> {
        self.skip_trivia();
        match self.s.peek() {
            Some(b'!') => {
                self.s.pos += 1;
                let e = self.primary_expression()?;
                Ok(self.push_expr(Expr::Not(e)))
            }
            Some(b'+') if !self.is_signed_numeric_start() => {
                self.s.pos += 1;
                let e = self.primary_expression()?;
                Ok(self.push_expr(Expr::Plus(e)))
            }
            Some(b'-') if !self.is_signed_numeric_start() => {
                self.s.pos += 1;
                let e = self.primary_expression()?;
                Ok(self.push_expr(Expr::Neg(e)))
            }
            _ => self.primary_expression(),
        }
    }
    fn function_call_tail(&mut self, iri: Iri) -> PResult<ExprId> {
        self.skip_trivia();
        if self.s.peek() == Some(b'(') {
            let (_distinct, args) = self.generic_arg_list()?;
            Ok(self.push_expr(Expr::FunctionCall(iri, args)))
        } else {
            Ok(self.push_expr(Expr::Term(Term::Iri(iri))))
        }
    }
    /// `ArgList`, minus the `NIL` shorthand (callers that allow a bare IRI
    /// with no parens at all check for `(` before calling this).
    fn generic_arg_list(&mut self) -> PResult<(bool, Range)> {
        self.s.expect(b'(')?;
        self.skip_trivia();
        let distinct = self.try_keyword(b"DISTINCT")?;
        self.skip_trivia();
        let start = self.q.expr_lists.start();
        if self.s.peek() != Some(b')') {
            loop {
                let e = self.expression()?;
                self.q.expr_lists.push(e);
                self.skip_trivia();
                if self.s.peek() == Some(b',') {
                    self.s.pos += 1;
                    self.skip_trivia();
                } else {
                    break;
                }
            }
        }
        self.s.expect(b')')?;
        Ok((distinct, self.q.expr_lists.range_from(start)))
    }
    /// Every `BuiltInCall` beyond `BOUND` shares one shape once parsed as a
    /// parser: a parenthesized (or `NIL`) comma-separated expression list.
    /// This parser does not enforce each function's exact arity — evaluating
    /// semantics, arity included, is left to the caller; see the crate's
    /// stance on staying a lexical/structural layer.
    fn built_in_call_args(&mut self, b: Builtin) -> PResult<ExprId> {
        self.skip_trivia();
        if b == Builtin::Bound {
            self.s.expect(b'(')?;
            self.skip_trivia();
            let v = self.var()?;
            self.skip_trivia();
            self.s.expect(b')')?;
            let arg = self.push_expr(Expr::Term(Term::Var(v)));
            let start = self.q.expr_lists.start();
            self.q.expr_lists.push(arg);
            let range = self.q.expr_lists.range_from(start);
            return Ok(self.push_expr(Expr::Call(b, range)));
        }
        let args = if self.s.try_nil()? {
            Range::new(self.q.expr_lists.start(), 0)
        } else {
            let (_distinct, args) = self.generic_arg_list()?;
            args
        };
        Ok(self.push_expr(Expr::Call(b, args)))
    }
    fn try_aggregate(&mut self, word: Span) -> PResult<Option<Aggregate>> {
        let kind = if word_eq_ci(self.s.bytes, word, b"COUNT") {
            AggKind::Count
        } else if word_eq_ci(self.s.bytes, word, b"SUM") {
            AggKind::Sum
        } else if word_eq_ci(self.s.bytes, word, b"MIN") {
            AggKind::Min
        } else if word_eq_ci(self.s.bytes, word, b"MAX") {
            AggKind::Max
        } else if word_eq_ci(self.s.bytes, word, b"AVG") {
            AggKind::Avg
        } else if word_eq_ci(self.s.bytes, word, b"SAMPLE") {
            AggKind::Sample
        } else if word_eq_ci(self.s.bytes, word, b"GROUP_CONCAT") {
            AggKind::GroupConcat
        } else {
            return Ok(None);
        };
        self.s.advance_to(word.end as usize);
        self.skip_trivia();
        self.s.expect(b'(')?;
        self.skip_trivia();
        let distinct = self.try_keyword(b"DISTINCT")?;
        self.skip_trivia();
        let expr = if kind == AggKind::Count && self.s.peek() == Some(b'*') {
            self.s.pos += 1;
            None
        } else {
            Some(self.expression()?)
        };
        self.skip_trivia();
        let separator = if kind == AggKind::GroupConcat && self.s.peek() == Some(b';') {
            self.s.pos += 1;
            self.skip_trivia();
            self.expect_keyword(b"SEPARATOR")?;
            self.skip_trivia();
            self.s.expect(b'=')?;
            self.skip_trivia();
            Some(self.s.string_literal()?)
        } else {
            None
        };
        self.skip_trivia();
        self.s.expect(b')')?;
        Ok(Some(Aggregate {
            kind,
            distinct,
            expr,
            separator,
        }))
    }
    fn primary_expression(&mut self) -> PResult<ExprId> {
        self.skip_trivia();
        match self.s.peek() {
            Some(b'(') => {
                self.s.pos += 1;
                let e = self.expression()?;
                self.skip_trivia();
                self.s.expect(b')')?;
                Ok(e)
            }
            Some(b'"' | b'\'') => {
                let t = self.rdf_literal()?;
                Ok(self.push_expr(Expr::Term(t)))
            }
            Some(b'?' | b'$') => {
                let v = self.var()?;
                Ok(self.push_expr(Expr::Term(Term::Var(v))))
            }
            Some(b) if b.is_ascii_digit() || matches!(b, b'.' | b'+' | b'-') => {
                match self.s.numeric_literal()? {
                    Some((kind, span)) => Ok(self.push_expr(Expr::Term(Term::Numeric(kind, span)))),
                    None => self.s.invalid("expected expression"),
                }
            }
            Some(b'<') => {
                let iri = Iri::Full(self.s.iriref()?);
                self.function_call_tail(iri)
            }
            _ => {
                let Some(word) = self.peek_ident()? else {
                    return self.s.invalid("expected expression");
                };
                if word_eq_ci(self.s.bytes, word, b"EXISTS") {
                    self.s.advance_to(word.end as usize);
                    self.skip_trivia();
                    let g = self.group_graph_pattern()?;
                    return Ok(self.push_expr(Expr::Exists(g)));
                }
                if word_eq_ci(self.s.bytes, word, b"NOT") {
                    self.s.advance_to(word.end as usize);
                    self.skip_trivia();
                    self.expect_keyword(b"EXISTS")?;
                    self.skip_trivia();
                    let g = self.group_graph_pattern()?;
                    return Ok(self.push_expr(Expr::NotExists(g)));
                }
                if word_eq_ci(self.s.bytes, word, b"TRUE") {
                    self.s.advance_to(word.end as usize);
                    return Ok(self.push_expr(Expr::Term(Term::Boolean(true))));
                }
                if word_eq_ci(self.s.bytes, word, b"FALSE") {
                    self.s.advance_to(word.end as usize);
                    return Ok(self.push_expr(Expr::Term(Term::Boolean(false))));
                }
                if let Some(agg) = self.try_aggregate(word)? {
                    return Ok(self.push_expr(Expr::Aggregate(agg)));
                }
                if let Some(b) = builtin_from_word(self.s.bytes, word) {
                    self.s.advance_to(word.end as usize);
                    return self.built_in_call_args(b);
                }
                if self.s.bytes.get(word.end as usize) == Some(&b':') {
                    let iri = self.prefixed_name_from(word)?;
                    return self.function_call_tail(iri);
                }
                self.s.invalid("expected expression")
            }
        }
    }
}

fn builtin_from_word(bytes: &[u8], span: Span) -> Option<Builtin> {
    let w = span.text(bytes);
    let eq = |s: &str| w.eq_ignore_ascii_case(s.as_bytes());
    Some(if eq("STR") {
        Builtin::Str
    } else if eq("LANG") {
        Builtin::Lang
    } else if eq("LANGMATCHES") {
        Builtin::LangMatches
    } else if eq("DATATYPE") {
        Builtin::Datatype
    } else if eq("BOUND") {
        Builtin::Bound
    } else if eq("IRI") || eq("URI") {
        Builtin::Iri
    } else if eq("BNODE") {
        Builtin::BNode
    } else if eq("RAND") {
        Builtin::Rand
    } else if eq("ABS") {
        Builtin::Abs
    } else if eq("CEIL") {
        Builtin::Ceil
    } else if eq("FLOOR") {
        Builtin::Floor
    } else if eq("ROUND") {
        Builtin::Round
    } else if eq("CONCAT") {
        Builtin::Concat
    } else if eq("SUBSTR") {
        Builtin::Substr
    } else if eq("STRLEN") {
        Builtin::StrLen
    } else if eq("REPLACE") {
        Builtin::Replace
    } else if eq("UCASE") {
        Builtin::UCase
    } else if eq("LCASE") {
        Builtin::LCase
    } else if eq("ENCODE_FOR_URI") {
        Builtin::EncodeForUri
    } else if eq("CONTAINS") {
        Builtin::Contains
    } else if eq("STRSTARTS") {
        Builtin::StrStarts
    } else if eq("STRENDS") {
        Builtin::StrEnds
    } else if eq("STRBEFORE") {
        Builtin::StrBefore
    } else if eq("STRAFTER") {
        Builtin::StrAfter
    } else if eq("YEAR") {
        Builtin::Year
    } else if eq("MONTH") {
        Builtin::Month
    } else if eq("DAY") {
        Builtin::Day
    } else if eq("HOURS") {
        Builtin::Hours
    } else if eq("MINUTES") {
        Builtin::Minutes
    } else if eq("SECONDS") {
        Builtin::Seconds
    } else if eq("TIMEZONE") {
        Builtin::Timezone
    } else if eq("TZ") {
        Builtin::Tz
    } else if eq("NOW") {
        Builtin::Now
    } else if eq("UUID") {
        Builtin::Uuid
    } else if eq("STRUUID") {
        Builtin::StrUuid
    } else if eq("MD5") {
        Builtin::Md5
    } else if eq("SHA1") {
        Builtin::Sha1
    } else if eq("SHA256") {
        Builtin::Sha256
    } else if eq("SHA384") {
        Builtin::Sha384
    } else if eq("SHA512") {
        Builtin::Sha512
    } else if eq("COALESCE") {
        Builtin::Coalesce
    } else if eq("IF") {
        Builtin::If
    } else if eq("STRLANG") {
        Builtin::StrLang
    } else if eq("STRDT") {
        Builtin::StrDt
    } else if eq("SAMETERM") {
        Builtin::SameTerm
    } else if eq("ISIRI") || eq("ISURI") {
        Builtin::IsIri
    } else if eq("ISBLANK") {
        Builtin::IsBlank
    } else if eq("ISLITERAL") {
        Builtin::IsLiteral
    } else if eq("ISNUMERIC") {
        Builtin::IsNumeric
    } else if eq("REGEX") {
        Builtin::Regex
    } else {
        return None;
    })
}

impl<'a> Parser<'a> {
    // ---- aggregate hoisting: see `Expr::Aggregate`'s doc comment ----

    fn hoist_in_range(&mut self, r: Range, out: &mut Vec<GroupAggregate>) {
        let ids: Vec<ExprId> = self.q.expr_ids(r).to_vec();
        for id in ids {
            self.hoist_in(id, out);
        }
    }
    fn hoist_in(&mut self, id: ExprId, out: &mut Vec<GroupAggregate>) {
        match self.q.expr(id) {
            Expr::Term(_) => {}
            Expr::Or(a, b)
            | Expr::And(a, b)
            | Expr::Eq(a, b)
            | Expr::Ne(a, b)
            | Expr::Lt(a, b)
            | Expr::Le(a, b)
            | Expr::Gt(a, b)
            | Expr::Ge(a, b)
            | Expr::Add(a, b)
            | Expr::Sub(a, b)
            | Expr::Mul(a, b)
            | Expr::Div(a, b) => {
                self.hoist_in(a, out);
                self.hoist_in(b, out);
            }
            Expr::Not(a) | Expr::Plus(a) | Expr::Neg(a) => self.hoist_in(a, out),
            Expr::In(a, list) | Expr::NotIn(a, list) => {
                self.hoist_in(a, out);
                self.hoist_in_range(list, out);
            }
            Expr::Call(_, args) | Expr::FunctionCall(_, args) => self.hoist_in_range(args, out),
            Expr::Aggregate(agg) => {
                let var = self.fresh_var();
                out.push(GroupAggregate { var, agg });
                *self.q.exprs.get_mut(id) = Expr::Term(Term::Var(var));
            }
            // EXISTS/NOT EXISTS introduce a fresh pattern scope; an aggregate
            // there would belong to that scope's own (unsupported, no nested
            // SELECT-less aggregation) evaluation, not to this GROUP BY.
            Expr::Exists(_) | Expr::NotExists(_) => {}
        }
    }

    // ---- solution modifiers ----

    fn starts_group_condition(&self) -> bool {
        match self.s.peek() {
            Some(b'?' | b'$' | b'(') => true,
            None | Some(b'}') => false,
            _ => match self.s.peek_word() {
                None => false,
                Some(w) => !["HAVING", "ORDER", "LIMIT", "OFFSET", "VALUES"]
                    .iter()
                    .any(|kw| word_eq_ci(self.s.bytes, w, kw.as_bytes())),
            },
        }
    }
    fn group_condition(&mut self, pattern: &mut NodeId) -> PResult<ExprId> {
        self.skip_trivia();
        if matches!(self.s.peek(), Some(b'?' | b'$')) {
            let v = self.var()?;
            return Ok(self.push_expr(Expr::Term(Term::Var(v))));
        }
        if self.s.peek() == Some(b'(') {
            self.s.pos += 1;
            let expr = self.expression()?;
            self.skip_trivia();
            let var = if self.try_keyword(b"AS")? {
                self.skip_trivia();
                self.var()?
            } else {
                self.fresh_var()
            };
            self.skip_trivia();
            self.s.expect(b')')?;
            *pattern = self.push_node(Algebra::Extend(*pattern, var, expr));
            return Ok(self.push_expr(Expr::Term(Term::Var(var))));
        }
        let expr = self.expression()?;
        let var = self.fresh_var();
        *pattern = self.push_node(Algebra::Extend(*pattern, var, expr));
        Ok(self.push_expr(Expr::Term(Term::Var(var))))
    }
    fn group_clause(&mut self, pattern: &mut NodeId) -> PResult<Vec<ExprId>> {
        self.skip_trivia();
        if !self.try_keyword(b"GROUP")? {
            return Ok(Vec::new());
        }
        self.skip_trivia();
        self.expect_keyword(b"BY")?;
        let mut keys = Vec::new();
        loop {
            self.skip_trivia();
            keys.push(self.group_condition(pattern)?);
            self.skip_trivia();
            if !self.starts_group_condition() {
                break;
            }
        }
        Ok(keys)
    }
    fn starts_constraint(&self) -> bool {
        match self.s.peek() {
            Some(b'(' | b'"' | b'\'' | b'?' | b'$' | b'<') => true,
            _ => match self.s.peek_word() {
                None => false,
                Some(w) => !["GROUP", "HAVING", "ORDER", "LIMIT", "OFFSET", "VALUES"]
                    .iter()
                    .any(|kw| word_eq_ci(self.s.bytes, w, kw.as_bytes())),
            },
        }
    }
    /// A `FILTER`/`HAVING` constraint: `BrackettedExpression | BuiltInCall | FunctionCall`,
    /// parsed as a general `Expression` — see this crate's stance on staying
    /// a structural layer rather than enforcing every grammar restriction.
    fn constraint(&mut self) -> PResult<ExprId> {
        self.expression()
    }
    fn having_clause(&mut self) -> PResult<Vec<ExprId>> {
        self.skip_trivia();
        if !self.try_keyword(b"HAVING")? {
            return Ok(Vec::new());
        }
        let mut exprs = Vec::new();
        self.skip_trivia();
        exprs.push(self.constraint()?);
        loop {
            self.skip_trivia();
            if !self.starts_constraint() {
                break;
            }
            exprs.push(self.constraint()?);
        }
        Ok(exprs)
    }
    fn order_condition(&mut self) -> PResult<OrderKey> {
        self.skip_trivia();
        if self.try_keyword(b"ASC")? {
            self.skip_trivia();
            self.s.expect(b'(')?;
            self.skip_trivia();
            let e = self.expression()?;
            self.skip_trivia();
            self.s.expect(b')')?;
            return Ok(OrderKey {
                expr: e,
                descending: false,
            });
        }
        if self.try_keyword(b"DESC")? {
            self.skip_trivia();
            self.s.expect(b'(')?;
            self.skip_trivia();
            let e = self.expression()?;
            self.skip_trivia();
            self.s.expect(b')')?;
            return Ok(OrderKey {
                expr: e,
                descending: true,
            });
        }
        let e = if matches!(self.s.peek(), Some(b'?' | b'$')) {
            let v = self.var()?;
            self.push_expr(Expr::Term(Term::Var(v)))
        } else {
            self.constraint()?
        };
        Ok(OrderKey {
            expr: e,
            descending: false,
        })
    }
    fn order_clause(&mut self) -> PResult<Vec<OrderKey>> {
        self.skip_trivia();
        if !self.try_keyword(b"ORDER")? {
            return Ok(Vec::new());
        }
        self.skip_trivia();
        self.expect_keyword(b"BY")?;
        let mut keys = Vec::new();
        self.skip_trivia();
        keys.push(self.order_condition()?);
        loop {
            self.skip_trivia();
            if !(self.starts_constraint() || matches!(self.s.peek(), Some(b'?' | b'$'))) {
                break;
            }
            keys.push(self.order_condition()?);
        }
        Ok(keys)
    }
    fn integer_literal(&mut self) -> PResult<u64> {
        let start = self.s.pos;
        while self.s.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.s.pos += 1;
        }
        if self.s.pos == self.s.bytes.len() && !self.s.eof {
            return Err(DecodeError::NeedMore);
        }
        if self.s.pos == start {
            return self.s.invalid("expected integer");
        }
        std::str::from_utf8(&self.s.bytes[start..self.s.pos])
            .ok()
            .and_then(|t| t.parse().ok())
            .ok_or(DecodeError::Invalid {
                offset: start,
                message: "integer literal out of range",
            })
    }
    fn limit_offset_clauses(&mut self) -> PResult<(Option<u64>, Option<u64>)> {
        self.skip_trivia();
        let mut limit = None;
        let mut offset = None;
        if self.try_keyword(b"LIMIT")? {
            self.skip_trivia();
            limit = Some(self.integer_literal()?);
            self.skip_trivia();
            if self.try_keyword(b"OFFSET")? {
                self.skip_trivia();
                offset = Some(self.integer_literal()?);
            }
        } else if self.try_keyword(b"OFFSET")? {
            self.skip_trivia();
            offset = Some(self.integer_literal()?);
            self.skip_trivia();
            if self.try_keyword(b"LIMIT")? {
                self.skip_trivia();
                limit = Some(self.integer_literal()?);
            }
        }
        Ok((limit, offset))
    }
    /// `GroupClause? HavingClause? OrderClause? LimitOffsetClauses?`, shared
    /// by every query form. `proj_extends` are a `SELECT`'s `(expr AS ?v)`
    /// items (empty for CONSTRUCT/ASK/DESCRIBE); they're `Extend`-wrapped
    /// between `HAVING` and `ORDER BY`, matching the spec's translation
    /// order, since `ORDER BY` may reference a variable they just bound.
    fn apply_solution_modifiers(
        &mut self,
        pattern: NodeId,
        proj_extends: &[(ExprId, Var)],
    ) -> PResult<NodeId> {
        let mut pattern = pattern;
        self.skip_trivia();
        let group_keys = self.group_clause(&mut pattern)?;
        self.skip_trivia();
        let having_exprs = self.having_clause()?;
        self.skip_trivia();
        let order_keys = self.order_clause()?;
        self.skip_trivia();
        let (limit, offset) = self.limit_offset_clauses()?;

        let mut roots: Vec<ExprId> = proj_extends.iter().map(|(e, _)| *e).collect();
        roots.extend(having_exprs.iter().copied());
        roots.extend(order_keys.iter().map(|k| k.expr));
        let mut aggregates = Vec::new();
        for r in roots {
            self.hoist_in(r, &mut aggregates);
        }

        let mut node = pattern;
        if !group_keys.is_empty() || !aggregates.is_empty() {
            let keys_start = self.q.expr_lists.start();
            for k in &group_keys {
                self.q.expr_lists.push(*k);
            }
            let keys = self.q.expr_lists.range_from(keys_start);
            let agg_start = self.q.group_aggregates.start();
            for a in aggregates {
                self.q.group_aggregates.push(a);
            }
            let aggregates = self.q.group_aggregates.range_from(agg_start);
            node = self.push_node(Algebra::Group {
                input: node,
                keys,
                aggregates,
            });
        }
        if let Some(e) = self.conjoin(having_exprs) {
            node = self.push_node(Algebra::Filter(e, node));
        }
        for &(e, v) in proj_extends {
            node = self.push_node(Algebra::Extend(node, v, e));
        }
        if !order_keys.is_empty() {
            let start = self.q.order_keys.start();
            for k in order_keys {
                self.q.order_keys.push(k);
            }
            let range = self.q.order_keys.range_from(start);
            node = self.push_node(Algebra::OrderBy(node, range));
        }
        if limit.is_some() || offset.is_some() {
            node = self.push_node(Algebra::Slice {
                input: node,
                offset,
                limit,
            });
        }
        Ok(node)
    }
    fn append_values(&mut self, node: NodeId) -> PResult<NodeId> {
        self.skip_trivia();
        if self.try_keyword(b"VALUES")? {
            self.skip_trivia();
            let values = self.inline_data()?;
            Ok(self.push_node(Algebra::Join(node, values)))
        } else {
            Ok(node)
        }
    }

    // ---- prologue, dataset, WHERE ----

    fn prologue(&mut self) -> PResult<()> {
        loop {
            self.skip_trivia();
            if self.try_keyword(b"BASE")? {
                self.skip_trivia();
                let span = self.s.iriref()?;
                self.q.prologue.base = Some(span);
            } else if self.try_keyword(b"PREFIX")? {
                self.skip_trivia();
                let prefix = if self.s.peek() == Some(b':') {
                    Span::new(self.s.pos as u32, self.s.pos as u32)
                } else {
                    let Some(word) = self.peek_ident()? else {
                        return self.s.invalid("expected prefix name");
                    };
                    self.s.advance_to(word.end as usize);
                    word
                };
                self.s.expect(b':')?;
                self.skip_trivia();
                let iri = self.s.iriref()?;
                self.q.prologue.prefixes.push((prefix, iri));
            } else {
                return Ok(());
            }
        }
    }
    fn dataset_clause(&mut self) -> PResult<bool> {
        self.skip_trivia();
        if !self.try_keyword(b"FROM")? {
            return Ok(false);
        }
        self.skip_trivia();
        if self.try_keyword(b"NAMED")? {
            self.skip_trivia();
            let iri = self.iri()?;
            self.q.dataset.named.push(iri);
        } else {
            let iri = self.iri()?;
            self.q.dataset.default.push(iri);
        }
        Ok(true)
    }
    fn where_clause(&mut self) -> PResult<NodeId> {
        self.skip_trivia();
        let _ = self.try_keyword(b"WHERE")?;
        self.skip_trivia();
        self.group_graph_pattern()
    }

    // ---- query forms ----

    fn select_clause(&mut self) -> PResult<(bool, bool, ProjectionSpec)> {
        self.skip_trivia();
        self.expect_keyword(b"SELECT")?;
        self.skip_trivia();
        let distinct = self.try_keyword(b"DISTINCT")?;
        let reduced = if !distinct {
            self.try_keyword(b"REDUCED")?
        } else {
            false
        };
        self.skip_trivia();
        if self.s.peek() == Some(b'*') {
            self.s.pos += 1;
            return Ok((distinct, reduced, ProjectionSpec::All));
        }
        let mut items = Vec::new();
        loop {
            self.skip_trivia();
            if matches!(self.s.peek(), Some(b'?' | b'$')) {
                items.push(ProjectionItem::Var(self.var()?));
            } else if self.s.peek() == Some(b'(') {
                self.s.pos += 1;
                self.skip_trivia();
                let e = self.expression()?;
                self.skip_trivia();
                self.expect_keyword(b"AS")?;
                self.skip_trivia();
                let v = self.var()?;
                self.skip_trivia();
                self.s.expect(b')')?;
                items.push(ProjectionItem::Expr(e, v));
            } else {
                break;
            }
            self.skip_trivia();
            if !matches!(self.s.peek(), Some(b'?' | b'$' | b'(')) {
                break;
            }
        }
        if items.is_empty() {
            return self.s.invalid("expected a projection variable list or '*'");
        }
        Ok((distinct, reduced, ProjectionSpec::Items(items)))
    }
    fn select_body(&mut self) -> PResult<(NodeId, bool, bool, Projection)> {
        let (distinct, reduced, spec) = self.select_clause()?;
        self.skip_trivia();
        while self.dataset_clause()? {
            self.skip_trivia();
        }
        self.skip_trivia();
        let pattern = self.where_clause()?;
        let proj_extends: Vec<(ExprId, Var)> = match &spec {
            ProjectionSpec::Items(items) => items
                .iter()
                .filter_map(|it| match it {
                    ProjectionItem::Expr(e, v) => Some((*e, *v)),
                    ProjectionItem::Var(_) => None,
                })
                .collect(),
            ProjectionSpec::All => Vec::new(),
        };
        let mut node = self.apply_solution_modifiers(pattern, &proj_extends)?;
        let projection = match spec {
            ProjectionSpec::All => Projection::All,
            ProjectionSpec::Items(items) => {
                let start = self.q.vars.start();
                for it in &items {
                    let v = match *it {
                        ProjectionItem::Var(v) => v,
                        ProjectionItem::Expr(_, v) => v,
                    };
                    self.q.vars.push(v);
                }
                let range = self.q.vars.range_from(start);
                node = self.push_node(Algebra::Project(node, range));
                Projection::Vars(range)
            }
        };
        if distinct {
            node = self.push_node(Algebra::Distinct(node));
        } else if reduced {
            node = self.push_node(Algebra::Reduced(node));
        }
        self.skip_trivia();
        node = self.append_values(node)?;
        Ok((node, distinct, reduced, projection))
    }
    fn sub_select(&mut self) -> PResult<NodeId> {
        let (node, ..) = self.select_body()?;
        Ok(node)
    }
    fn select_top(&mut self) -> PResult<()> {
        let (node, distinct, reduced, projection) = self.select_body()?;
        self.q.form = Some(QueryForm::Select {
            distinct,
            reduced,
            projection,
        });
        self.q.root = Some(node);
        Ok(())
    }
    fn construct_template(&mut self) -> PResult<Range> {
        self.s.expect(b'{')?;
        self.skip_trivia();
        let mut items = Vec::new();
        if self.s.peek() != Some(b'}') {
            self.triples_same_subject_path(&mut items)?;
            loop {
                self.skip_trivia();
                if self.s.peek() != Some(b'.') {
                    break;
                }
                self.s.pos += 1;
                self.skip_trivia();
                if self.s.peek() == Some(b'}') {
                    break;
                }
                self.triples_same_subject_path(&mut items)?;
            }
        }
        self.skip_trivia();
        self.s.expect(b'}')?;
        let start = self.q.triples.start();
        for item in items {
            match item {
                RawTriple::Simple(t) => {
                    self.q.triples.push(t);
                }
                RawTriple::Complex(..) => {
                    return self
                        .s
                        .invalid("property paths are not allowed in a CONSTRUCT template");
                }
            }
        }
        Ok(self.q.triples.range_from(start))
    }
    fn construct_query(&mut self) -> PResult<()> {
        self.skip_trivia();
        self.expect_keyword(b"CONSTRUCT")?;
        self.skip_trivia();
        let (template, pattern) = if self.s.peek() == Some(b'{') {
            let template = self.construct_template()?;
            self.skip_trivia();
            while self.dataset_clause()? {
                self.skip_trivia();
            }
            self.skip_trivia();
            let pattern = self.where_clause()?;
            (template, pattern)
        } else {
            while self.dataset_clause()? {
                self.skip_trivia();
            }
            self.skip_trivia();
            self.expect_keyword(b"WHERE")?;
            self.skip_trivia();
            let template = self.construct_template()?;
            let pattern = self.push_node(Algebra::Bgp(template));
            (template, pattern)
        };
        self.skip_trivia();
        let node = self.apply_solution_modifiers(pattern, &[])?;
        self.q.form = Some(QueryForm::Construct { template });
        self.q.root = Some(node);
        Ok(())
    }
    fn ask_query(&mut self) -> PResult<()> {
        self.skip_trivia();
        self.expect_keyword(b"ASK")?;
        self.skip_trivia();
        while self.dataset_clause()? {
            self.skip_trivia();
        }
        self.skip_trivia();
        let pattern = self.where_clause()?;
        self.skip_trivia();
        let node = self.apply_solution_modifiers(pattern, &[])?;
        self.q.form = Some(QueryForm::Ask);
        self.q.root = Some(node);
        Ok(())
    }
    fn describe_query(&mut self) -> PResult<()> {
        self.skip_trivia();
        self.expect_keyword(b"DESCRIBE")?;
        self.skip_trivia();
        let targets_start = self.q.terms.start();
        if self.s.peek() == Some(b'*') {
            self.s.pos += 1;
        } else {
            loop {
                self.skip_trivia();
                let t = if matches!(self.s.peek(), Some(b'?' | b'$')) {
                    Term::Var(self.var()?)
                } else {
                    Term::Iri(self.iri()?)
                };
                self.q.terms.push(t);
                self.skip_trivia();
                if !matches!(self.s.peek(), Some(b'?' | b'$' | b'<'))
                    && self.s.peek_word().is_none()
                {
                    break;
                }
            }
        }
        let targets = self.q.terms.range_from(targets_start);
        self.skip_trivia();
        while self.dataset_clause()? {
            self.skip_trivia();
        }
        self.skip_trivia();
        let has_pattern = self.try_keyword(b"WHERE")? || self.s.peek() == Some(b'{');
        let pattern = if has_pattern {
            self.skip_trivia();
            Some(self.group_graph_pattern()?)
        } else {
            None
        };
        self.skip_trivia();
        let node = match pattern {
            Some(p) => Some(self.apply_solution_modifiers(p, &[])?),
            None => None,
        };
        self.q.form = Some(QueryForm::Describe {
            targets,
            pattern: node,
        });
        self.q.root = node;
        Ok(())
    }
    fn query(&mut self) -> PResult<()> {
        self.skip_trivia();
        let Some(word) = self.peek_ident()? else {
            return self
                .s
                .invalid("expected SELECT, CONSTRUCT, DESCRIBE, or ASK");
        };
        if word_eq_ci(self.s.bytes, word, b"SELECT") {
            self.select_top()
        } else if word_eq_ci(self.s.bytes, word, b"CONSTRUCT") {
            self.construct_query()
        } else if word_eq_ci(self.s.bytes, word, b"DESCRIBE") {
            self.describe_query()
        } else if word_eq_ci(self.s.bytes, word, b"ASK") {
            self.ask_query()
        } else {
            self.s
                .invalid("expected SELECT, CONSTRUCT, DESCRIBE, or ASK")
        }
    }

    /// Parse one complete `QueryUnit` from the start of the buffer, returning
    /// the finished arena and how many bytes it consumed.
    pub fn parse_query_unit(mut self) -> PResult<(QueryData, usize)> {
        self.skip_trivia();
        self.prologue()?;
        self.skip_trivia();
        self.query()?;
        Ok((self.q, self.s.pos))
    }
}

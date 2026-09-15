//! Property path expressions (SPARQL 1.1 §9). Stored in [`super::algebra::Query`]'s
//! `paths` pool and addressed by [`super::ids::PathId`], the same
//! index-instead-of-`Box` arrangement `Expr` and `Algebra` use.
use super::ids::PathId;
use super::pool::Range;
use super::term::Iri;

/// One `!`-negated property set member: `iri` or `^iri`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NegPathItem {
    pub inverse: bool,
    pub iri: Iri,
}

#[derive(Clone, Copy, Debug)]
pub enum PathExpr {
    /// A plain IRI, or `a` (rdf:type).
    Link(Iri),
    Inv(PathId),
    Seq(PathId, PathId),
    Alt(PathId, PathId),
    ZeroOrMore(PathId),
    OneOrMore(PathId),
    ZeroOrOne(PathId),
    /// `!(iri1|^iri2|...)`; items live in `Query.neg_path_items`.
    NegatedPropertySet(Range),
}

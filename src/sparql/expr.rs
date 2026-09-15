//! Scalar expressions (SPARQL 1.1 §17). Nodes live in [`super::algebra::Query`]'s
//! `exprs` pool and reference each other by [`super::ids::ExprId`] instead of
//! `Box`, so the tree stays one flat, `Copy`-element allocation.
use super::ids::{ExprId, NodeId};
use super::pool::Range;
use super::term::{Iri, Span, Term, Var};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    Str,
    Lang,
    LangMatches,
    Datatype,
    Bound,
    Iri,
    BNode,
    Rand,
    Abs,
    Ceil,
    Floor,
    Round,
    Concat,
    Substr,
    StrLen,
    Replace,
    UCase,
    LCase,
    EncodeForUri,
    Contains,
    StrStarts,
    StrEnds,
    StrBefore,
    StrAfter,
    Year,
    Month,
    Day,
    Hours,
    Minutes,
    Seconds,
    Timezone,
    Tz,
    Now,
    Uuid,
    StrUuid,
    Md5,
    Sha1,
    Sha256,
    Sha384,
    Sha512,
    Coalesce,
    If,
    StrLang,
    StrDt,
    SameTerm,
    IsIri,
    IsBlank,
    IsLiteral,
    IsNumeric,
    Regex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggKind {
    Count,
    Sum,
    Min,
    Max,
    Avg,
    Sample,
    GroupConcat,
}

/// An aggregate hoisted out of a `Group`'s scope during translation; see
/// [`super::algebra::Algebra::Group`]. `expr` is `None` for `COUNT(*)`.
#[derive(Clone, Copy, Debug)]
pub struct Aggregate {
    pub kind: AggKind,
    pub distinct: bool,
    pub expr: Option<ExprId>,
    /// `SEPARATOR` string content for `GROUP_CONCAT`, if given.
    pub separator: Option<Span>,
}

/// A hoisted aggregate bound to the synthetic (or, for a bare `SELECT (COUNT(*)
/// AS ?c)`, named) variable `Group` produces it under.
#[derive(Clone, Copy, Debug)]
pub struct GroupAggregate {
    pub var: Var,
    pub agg: Aggregate,
}

#[derive(Clone, Copy, Debug)]
pub enum Expr {
    Term(Term),
    Or(ExprId, ExprId),
    And(ExprId, ExprId),
    Not(ExprId),
    Eq(ExprId, ExprId),
    Ne(ExprId, ExprId),
    Lt(ExprId, ExprId),
    Le(ExprId, ExprId),
    Gt(ExprId, ExprId),
    Ge(ExprId, ExprId),
    In(ExprId, Range),
    NotIn(ExprId, Range),
    Add(ExprId, ExprId),
    Sub(ExprId, ExprId),
    Mul(ExprId, ExprId),
    Div(ExprId, ExprId),
    /// Unary `+`.
    Plus(ExprId),
    /// Unary `-`.
    Neg(ExprId),
    Call(Builtin, Range),
    /// A custom/extension function call: `iri(args...)`.
    FunctionCall(Iri, Range),
    /// Parsed where an aggregate is grammatically allowed (SELECT expressions,
    /// HAVING, ORDER BY). Translation hoists each occurrence into the
    /// enclosing `Group`'s aggregate list and overwrites this pool slot with
    /// `Expr::Term(Term::Var(_))` referencing the synthetic result variable —
    /// see `sparql::parser`'s aggregate-hoisting pass. A `Group`'s own
    /// `aggregates` range holds the post-hoist [`GroupAggregate`] instead.
    Aggregate(Aggregate),
    Exists(NodeId),
    NotExists(NodeId),
}

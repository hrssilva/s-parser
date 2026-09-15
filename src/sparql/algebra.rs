//! The SPARQL algebra tree (§18.2 of the spec) that parsing produces, and
//! [`QueryData`], the arena that owns it. Every node kind here is `Copy` and
//! refers to its children by [`NodeId`]/[`ExprId`]/[`PathId`] rather than
//! `Box`, so a whole query is one set of flat pools instead of a tree of heap
//! allocations — this is [`crate::tokenizer::Tokenizer::Output`] for
//! [`super::SparqlTokenizer`], the same role `[Token; 4]` plays for
//! N-Triples, just variable-size instead of fixed.
use super::expr::{Expr, GroupAggregate};
use super::ids::{ExprId, NodeId, PathId};
use super::path::{NegPathItem, PathExpr};
use super::pool::{Pool, Range};
use super::term::{Iri, Prologue, Term, Var};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TriplePattern {
    pub subject: Term,
    pub predicate: Term,
    pub object: Term,
}

#[derive(Clone, Copy, Debug)]
pub struct OrderKey {
    pub expr: ExprId,
    pub descending: bool,
}

/// One SPARQL algebra operator. Trees are built bottom-up during parsing and
/// never mutated afterward, except that aggregate-hoisting overwrites
/// `Expr::Aggregate` pool slots in place (see [`super::expr::Expr::Aggregate`]);
/// `NodeId`s themselves stay stable throughout.
#[derive(Clone, Copy, Debug)]
pub enum Algebra {
    /// A block of simple (non-path) triples; range into `Query.triples`.
    Bgp(Range),
    /// A single triple whose predicate is a property path.
    Path {
        subject: Term,
        path: PathId,
        object: Term,
    },
    Join(NodeId, NodeId),
    /// `OPTIONAL`. The third field is the join *condition* (default `true`),
    /// evaluated against the combined row — not a plain `Filter` on the right
    /// side alone, because it can reference variables from the left. See
    /// `sparql::parser`'s translation of `OPTIONAL { P FILTER F }`.
    LeftJoin(NodeId, NodeId, Option<ExprId>),
    Union(NodeId, NodeId),
    Minus(NodeId, NodeId),
    Graph(Term, NodeId),
    Filter(ExprId, NodeId),
    /// `BIND(expr AS ?var)`, and each `GROUP BY (expr AS ?var)` key.
    Extend(NodeId, Var, ExprId),
    Group {
        input: NodeId,
        /// Range into `Query.expr_lists`; empty for an implicit whole-group.
        keys: Range,
        /// Range into `Query.group_aggregates`.
        aggregates: Range,
    },
    /// Range into `Query.order_keys`.
    OrderBy(NodeId, Range),
    /// Range into `Query.vars`. Absent for `SELECT *` — see [`Projection`].
    Project(NodeId, Range),
    Distinct(NodeId),
    Reduced(NodeId),
    Slice {
        input: NodeId,
        offset: Option<u64>,
        limit: Option<u64>,
    },
    /// `VALUES`/inline data. `rows.len == vars.len * row_count`, row-major.
    Table {
        vars: Range,
        rows: Range,
    },
    /// The join identity: a pattern with exactly one empty solution, e.g. the
    /// implicit input to a `VALUES`-only or filter-only group.
    Unit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Projection {
    /// `SELECT *`. Computing the projected variable set from the pattern is
    /// left to the caller, consistent with this crate staying a lexical/
    /// structural layer — see the README's stance on N-Triples semantics.
    All,
    /// Range into `Query.vars`, in written order; matches the accompanying
    /// `Project` node's own range when one is emitted.
    Vars(Range),
}

#[derive(Clone, Debug, Default)]
pub struct Dataset {
    pub default: Vec<Iri>,
    pub named: Vec<Iri>,
}

#[derive(Clone, Copy, Debug)]
pub enum QueryForm {
    Select {
        distinct: bool,
        reduced: bool,
        projection: Projection,
    },
    /// Range into `Query.triples`.
    Construct {
        template: Range,
    },
    Ask,
    /// Range into `Query.terms` (each an `Iri` or a `Var`). `pattern` is
    /// `None` for a WHERE-less `DESCRIBE <a> <b>`.
    Describe {
        targets: Range,
        pattern: Option<NodeId>,
    },
}

/// The complete parse of one `QueryUnit`: an owned, lifetime-free arena
/// (every span is a `u32` offset, not a borrow) plus the algebra tree over
/// it. [`crate::reader::query_reader::Query`] pairs this with the source
/// bytes those offsets index into.
#[derive(Clone, Debug, Default)]
pub struct QueryData {
    pub prologue: Prologue,
    pub dataset: Dataset,
    pub form: Option<QueryForm>,
    /// The WHERE pattern's root, after all solution modifiers are applied.
    /// `None` only transiently before the parser sets it.
    pub root: Option<NodeId>,
    pub(super) nodes: Pool<Algebra>,
    pub(super) exprs: Pool<Expr>,
    pub(super) paths: Pool<PathExpr>,
    pub(super) triples: Pool<TriplePattern>,
    pub(super) vars: Pool<Var>,
    pub(super) expr_lists: Pool<ExprId>,
    pub(super) order_keys: Pool<OrderKey>,
    pub(super) group_aggregates: Pool<GroupAggregate>,
    pub(super) values: Pool<Option<Term>>,
    pub(super) terms: Pool<Term>,
    pub(super) neg_path_items: Pool<NegPathItem>,
}

impl QueryData {
    pub fn node(&self, id: NodeId) -> Algebra {
        self.nodes[id as usize]
    }
    pub fn expr(&self, id: ExprId) -> Expr {
        self.exprs[id as usize]
    }
    pub fn path(&self, id: PathId) -> PathExpr {
        self.paths[id as usize]
    }
    pub fn triples(&self, range: Range) -> &[TriplePattern] {
        self.triples.get(range)
    }
    pub fn vars(&self, range: Range) -> &[Var] {
        self.vars.get(range)
    }
    /// The `ExprId`s a `Group`'s `keys`, an `In`/`NotIn`, or a `Call`/
    /// `FunctionCall`'s arguments range over.
    pub fn expr_ids(&self, range: Range) -> &[ExprId] {
        self.expr_lists.get(range)
    }
    pub fn order_keys(&self, range: Range) -> &[OrderKey] {
        self.order_keys.get(range)
    }
    pub fn group_aggregates(&self, range: Range) -> &[GroupAggregate] {
        self.group_aggregates.get(range)
    }
    /// `VALUES`/`Table` row data, `None` for `UNDEF`.
    pub fn values(&self, range: Range) -> &[Option<Term>] {
        self.values.get(range)
    }
    /// `DESCRIBE` targets.
    pub fn terms(&self, range: Range) -> &[Term] {
        self.terms.get(range)
    }
    pub fn neg_path_items(&self, range: Range) -> &[NegPathItem] {
        self.neg_path_items.get(range)
    }
}

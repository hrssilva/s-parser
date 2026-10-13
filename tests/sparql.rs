use s_parser::parse_query;
use s_parser::sparql::algebra::{Algebra, QueryForm};
use s_parser::sparql::expr::Expr;
use s_parser::sparql::term::{Iri, Term, Var};

fn ok(src: &str) -> s_parser::Query<'_> {
    match parse_query(src.as_bytes()) {
        Ok(q) => q,
        Err(e) => panic!("expected {src:?} to parse, got {e:?}"),
    }
}
fn err(src: &str) {
    if parse_query(src.as_bytes()).is_ok() {
        panic!("expected {src:?} to fail to parse");
    }
}

#[test]
fn simple_select_bgp() {
    let q = ok("SELECT ?s ?o WHERE { ?s <urn:p> ?o . ?o <urn:q> ?s }");
    let data = q.data();
    let Some(QueryForm::Select {
        distinct: false,
        reduced: false,
        ..
    }) = data.form
    else {
        panic!("expected a plain SELECT form")
    };
    let root = data.root.expect("root set");
    // Project(...) wraps the joined BGPs.
    let Algebra::Project(inner, vars) = data.node(root) else {
        panic!("expected Project at the root")
    };
    let vars = data.vars(vars);
    assert_eq!(vars.len(), 2);
    // Two simple triples in one TriplesBlock merge into a single Bgp.
    let Algebra::Bgp(range) = data.node(inner) else {
        panic!("expected a single merged Bgp, got {:?}", data.node(inner))
    };
    let triples = data.triples(range);
    assert_eq!(triples.len(), 2);
    let Term::Var(Var::Named(s)) = triples[0].subject else {
        panic!("expected a variable subject")
    };
    assert_eq!(s.text(q.bytes()), b"s");
}

#[test]
fn prefixed_names_resolve_without_allocating() {
    let q = ok("PREFIX ex: <http://example.org/> SELECT ?s WHERE { ?s a ex:Thing }");
    let data = q.data();
    let root = data.root.unwrap();
    let Algebra::Project(inner, _) = data.node(root) else {
        panic!()
    };
    let Algebra::Bgp(range) = data.node(inner) else {
        panic!()
    };
    let triple = data.triples(range)[0];
    let Term::Iri(iri @ Iri::WellKnown(_)) = triple.predicate else {
        panic!(
            "expected 'a' to resolve to rdf:type, got {:?}",
            triple.predicate
        )
    };
    let resolved = data.prologue.resolve(iri, q.bytes()).unwrap();
    let (a, b) = resolved.parts();
    assert_eq!(
        [a, b].concat(),
        b"http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
    );

    let Term::Iri(obj_iri) = triple.object else {
        panic!()
    };
    let resolved = q.resolve(obj_iri).unwrap();
    let (a, b) = resolved.parts();
    assert_eq!([a, b].concat(), b"http://example.org/Thing");
}

#[test]
fn optional_filter_references_outer_variable() {
    // The FILTER inside OPTIONAL references ?v from the left side, so it
    // must become LeftJoin's own condition, not a Filter nested inside the
    // right-hand pattern (which wouldn't see ?v at all).
    let q = ok("SELECT * WHERE { ?x <urn:p> ?v OPTIONAL { ?x <urn:q> ?w FILTER(?v = ?w) } }");
    let data = q.data();
    let root = data.root.unwrap();
    // SELECT * emits no Project node.
    let Algebra::LeftJoin(_left, _right, cond) = data.node(root) else {
        panic!("expected LeftJoin at the root, got {:?}", data.node(root))
    };
    let cond = cond.expect("OPTIONAL's FILTER should attach to LeftJoin, not be lost");
    assert!(matches!(data.expr(cond), Expr::Eq(_, _)));
}

#[test]
fn union_and_minus() {
    let q =
        ok("SELECT * WHERE { { ?s <urn:a> ?o } UNION { ?s <urn:b> ?o } MINUS { ?s <urn:c> ?o } }");
    let data = q.data();
    let root = data.root.unwrap();
    let Algebra::Minus(left, _right) = data.node(root) else {
        panic!("expected Minus at the root, got {:?}", data.node(root))
    };
    assert!(matches!(data.node(left), Algebra::Union(_, _)));
}

#[test]
fn property_path_is_not_a_bgp_triple() {
    let q = ok("SELECT * WHERE { ?s <urn:a>/<urn:b>+ ?o }");
    let data = q.data();
    let root = data.root.unwrap();
    let Algebra::Path { path, .. } = data.node(root) else {
        panic!("expected a Path node, got {:?}", data.node(root))
    };
    // (a / b+) => Seq(Link(a), OneOrMore(Link(b)))
    assert!(matches!(
        data.path(path),
        s_parser::sparql::path::PathExpr::Seq(_, _)
    ));
}

#[test]
fn aggregates_and_group_by_hoist_correctly() {
    let q = ok(
        "SELECT ?s (COUNT(?o) AS ?n) WHERE { ?s <urn:p> ?o } GROUP BY ?s HAVING (COUNT(?o) > 1) ORDER BY DESC(?n)",
    );
    let data = q.data();
    let root = data.root.unwrap();
    // Project -> OrderBy -> Extend(?n AS the hoisted COUNT(?o) result) -> Filter(Having) -> Group,
    // matching the spec's translation order (18.2.5).
    let Algebra::Project(after_order, _) = data.node(root) else {
        panic!("expected Project at root, got {:?}", data.node(root))
    };
    let Algebra::OrderBy(after_extend, _) = data.node(after_order) else {
        panic!("expected OrderBy, got {:?}", data.node(after_order))
    };
    let Algebra::Extend(after_having, _, _) = data.node(after_extend) else {
        panic!(
            "expected Extend for (COUNT(?o) AS ?n), got {:?}",
            data.node(after_extend)
        )
    };
    let Algebra::Filter(_having_expr, group_node) = data.node(after_having) else {
        panic!(
            "expected Filter (HAVING), got {:?}",
            data.node(after_having)
        )
    };
    let Algebra::Group {
        aggregates, keys, ..
    } = data.node(group_node)
    else {
        panic!("expected Group, got {:?}", data.node(group_node))
    };
    assert_eq!(keys.len, 1);
    // COUNT(?o) appears in both the SELECT list and HAVING: each occurrence
    // is its own ExprId, so hoisting produces one aggregate entry per
    // occurrence rather than deduplicating equivalent aggregates.
    assert_eq!(data.group_aggregates(aggregates).len(), 2);
}

#[test]
fn construct_and_ask_and_describe() {
    let q = ok("CONSTRUCT { ?s <urn:p2> ?o } WHERE { ?s <urn:p> ?o }");
    assert!(matches!(q.data().form, Some(QueryForm::Construct { .. })));

    let q = ok("ASK { ?s <urn:p> ?o }");
    assert!(matches!(q.data().form, Some(QueryForm::Ask)));

    let q = ok("DESCRIBE <urn:x>");
    let Some(QueryForm::Describe { targets, pattern }) = q.data().form else {
        panic!()
    };
    assert_eq!(targets.len, 1);
    assert!(pattern.is_none());
}

#[test]
fn values_clause_supports_undef() {
    let q = ok("SELECT * WHERE { VALUES (?x ?y) { (1 UNDEF) (2 3) } }");
    let data = q.data();
    let root = data.root.unwrap();
    let Algebra::Table { vars, rows } = data.node(root) else {
        panic!("expected a Table node, got {:?}", data.node(root))
    };
    assert_eq!(vars.len, 2);
    let values = data.values(rows);
    assert_eq!(values.len(), 4);
    assert!(values[1].is_none()); // UNDEF
}

#[test]
fn empty_prefix_names_resolve_correctly() {
    // `PNAME_NS ::= PN_PREFIX? ':'` — the prefix itself is optional, so the
    // default/empty prefix (`PREFIX : <...>` then bare `:x`) is ordinary,
    // spec-legal SPARQL, not an edge case.
    let q = ok("PREFIX : <http://example/> ASK { :x :p 1 }");
    let data = q.data();
    let root = data.root.unwrap();
    let Algebra::Bgp(range) = data.node(root) else {
        panic!("expected a Bgp, got {:?}", data.node(root))
    };
    let triple = data.triples(range)[0];
    let Term::Iri(subject_iri) = triple.subject else {
        panic!("expected an IRI subject, got {:?}", triple.subject)
    };
    let resolved = q.resolve(subject_iri).unwrap();
    let (a, b) = resolved.parts();
    assert_eq!([a, b].concat(), b"http://example/x");

    // The same empty prefix inside a property path, and as a verb after `;`.
    let q = ok("PREFIX : <http://example/> SELECT * WHERE { ?s :p/:q ?o }");
    let data = q.data();
    let root = data.root.unwrap();
    assert!(matches!(data.node(root), Algebra::Path { .. }));

    ok("PREFIX : <http://example/> SELECT * WHERE { ?s :p :o ; :q :r }");
    // A bare `:` (empty prefix, empty local) alone is also a valid IRI.
    ok("PREFIX : <http://example/> SELECT * WHERE { : :p :o }");
}

#[test]
fn rejects_service() {
    err("SELECT * WHERE { SERVICE <urn:x> { ?s ?p ?o } }");
}

#[test]
fn rejects_trailing_content_after_one_query() {
    err("SELECT * WHERE { ?s ?p ?o } SELECT * WHERE { ?s ?p ?o }");
}

#[test]
fn window_boundaries_need_more_then_succeed() {
    let src = b"PREFIX ex: <http://example.org/> SELECT ?s WHERE { ?s a ex:Thing . ?s <urn:p> \"hello world\"@en }";
    for window in 1..=src.len() {
        let stream = s_parser::SliceStream::new(src, window).unwrap();
        let mut reader = s_parser::QueryReader::new(stream, s_parser::SparqlTokenizer);
        match reader.next() {
            Ok(Some(query)) => {
                assert_eq!(query.bytes(), src.as_slice());
                assert!(reader.next().unwrap().is_none());
            }
            // A fixed-size window smaller than the whole query can never fit
            // it, the same way an oversized N-Triples record can't.
            Err(s_parser::ParseError::RecordTooLong { .. }) if window < src.len() => {}
            other => panic!("window {window}: unexpected result {other:?}"),
        }
    }
}

#[test]
fn nested_call_arguments_stay_in_their_own_lists() {
    use s_parser::sparql::algebra::Algebra;
    use s_parser::sparql::expr::{Builtin, Expr};
    let q = ok("SELECT * WHERE { ?s ?p ?o FILTER(CONCAT(\"[\", STR(?o), \"]\") != COUNT(?x)) }");
    let data = q.data();
    let root = data.root.unwrap();
    let Algebra::Filter(f, _) = data.node(root) else { panic!("{:?}", data.node(root)) };
    let Expr::Ne(concat, _) = data.expr(f) else { panic!() };
    let Expr::Call(Builtin::Concat, args) = data.expr(concat) else { panic!() };
    let args = data.expr_ids(args);
    assert_eq!(args.len(), 3);
    assert!(matches!(data.expr(args[1]), Expr::Call(Builtin::Str, inner) if data.expr_ids(inner).len() == 1));
}

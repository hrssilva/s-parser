use s_parser::turtle::term::{BlankNode, Iri, LiteralSuffix, Term, WellKnown};
use s_parser::{ParseError, SliceStream, TurtleReader, TurtleTokenizer};

fn collect(data: &[u8], window: usize) -> Result<Vec<Vec<u8>>, ParseError> {
    let mut reader = TurtleReader::new(
        SliceStream::new(data, window).unwrap(),
        TurtleTokenizer::default(),
    );
    let mut records = Vec::new();
    while let Some(stmt) = reader.next()? {
        records.push(stmt.bytes().to_vec());
    }
    Ok(records)
}

fn iri_text<'a>(iri: Iri, record: &'a [u8], buf: &'a mut Vec<u8>) -> &'a [u8] {
    buf.clear();
    iri.write_to(record, buf);
    buf.as_slice()
}

#[test]
fn simple_triple() {
    let src = b"<urn:s> <urn:p> <urn:o> .";
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 256).unwrap(),
        TurtleTokenizer::default(),
    );
    let stmt = reader.next().unwrap().unwrap();
    let triples = stmt.triples();
    assert_eq!(triples.len(), 1);
    let Term::Iri(Iri::Full(s)) = triples[0].subject else {
        panic!("expected full IRI subject")
    };
    assert_eq!(s.text(stmt.bytes()), b"urn:s");
    assert!(reader.next().unwrap().is_none());
}

#[test]
fn prefix_directive_produces_no_record_and_resolves_later() {
    let src = b"@prefix ex: <http://example.org/> .\nex:s ex:p ex:o .";
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 256).unwrap(),
        TurtleTokenizer::default(),
    );
    let stmt = reader.next().unwrap().unwrap();
    let triples = stmt.triples();
    assert_eq!(triples.len(), 1);
    let Term::Iri(iri) = triples[0].subject else {
        panic!()
    };
    let mut buf = Vec::new();
    assert_eq!(
        iri_text(iri, stmt.bytes(), &mut buf),
        b"http://example.org/s"
    );
    assert!(reader.next().unwrap().is_none());
}

#[test]
fn empty_prefix_resolves() {
    let src = b"@prefix : <http://example.org/> .\n:s :p :o .";
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 256).unwrap(),
        TurtleTokenizer::default(),
    );
    let stmt = reader.next().unwrap().unwrap();
    let Term::Iri(iri) = stmt.triples()[0].predicate else {
        panic!()
    };
    let mut buf = Vec::new();
    assert_eq!(
        iri_text(iri, stmt.bytes(), &mut buf),
        b"http://example.org/p"
    );
}

#[test]
fn sparql_style_prefix_directive() {
    let src = b"PREFIX ex: <http://example.org/>\nex:s ex:p ex:o .";
    let records = collect(src, 256).unwrap();
    assert_eq!(records.len(), 1);
}

#[test]
fn prefix_redeclared_later_in_document() {
    let src = b"@prefix ex: <http://a.org/> .\nex:s ex:p ex:o .\n@prefix ex: <http://b.org/> .\nex:s2 ex:p2 ex:o2 .";
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 256).unwrap(),
        TurtleTokenizer::default(),
    );
    let first = reader.next().unwrap().unwrap();
    let Term::Iri(iri) = first.triples()[0].subject else {
        panic!()
    };
    let mut buf = Vec::new();
    assert_eq!(iri_text(iri, first.bytes(), &mut buf), b"http://a.org/s");

    let second = reader.next().unwrap().unwrap();
    let Term::Iri(iri) = second.triples()[0].subject else {
        panic!()
    };
    assert_eq!(iri_text(iri, second.bytes(), &mut buf), b"http://b.org/s2");
}

#[test]
fn undeclared_prefix_is_an_error() {
    let src = b"ex:s ex:p ex:o .";
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 256).unwrap(),
        TurtleTokenizer::default(),
    );
    assert!(reader.next().is_err());
}

#[test]
fn base_directive_is_parsed_and_skipped() {
    let src = b"@base <http://example.org/> .\n<s> <p> <o> .";
    let records = collect(src, 256).unwrap();
    assert_eq!(records.len(), 1);

    let src = b"BASE <http://example.org/>\n<s> <p> <o> .";
    let records = collect(src, 256).unwrap();
    assert_eq!(records.len(), 1);
}

#[test]
fn a_keyword_resolves_to_rdf_type() {
    let src = b"<urn:s> a <urn:Thing> .";
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 256).unwrap(),
        TurtleTokenizer::default(),
    );
    let stmt = reader.next().unwrap().unwrap();
    let Term::Iri(Iri::WellKnown(WellKnown::RdfType)) = stmt.triples()[0].predicate else {
        panic!("expected 'a' to resolve to rdf:type")
    };
}

#[test]
fn semicolon_and_comma_lists_expand() {
    let src = b"<urn:s> <urn:p1> <urn:o1>, <urn:o2> ; <urn:p2> <urn:o3> .";
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 256).unwrap(),
        TurtleTokenizer::default(),
    );
    let stmt = reader.next().unwrap().unwrap();
    assert_eq!(stmt.triples().len(), 3);
}

#[test]
fn blank_node_property_list_as_object() {
    let src = b"<urn:s> <urn:p> [ <urn:q> <urn:r> ] .";
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 256).unwrap(),
        TurtleTokenizer::default(),
    );
    let stmt = reader.next().unwrap().unwrap();
    let triples = stmt.triples();
    assert_eq!(triples.len(), 2);
    // The nested `[ ... ]` property list is parsed (and its triple emitted)
    // before the outer object_list call that references it as `object`, so
    // it appears first in the buffer.
    let Term::Blank(b) = triples[0].subject else {
        panic!("expected the blank node as subject of the nested triple")
    };
    let Term::Blank(b2) = triples[1].object else {
        panic!("expected the same blank node as object of the outer triple")
    };
    assert_eq!(b, b2);
}

#[test]
fn bare_anon_is_a_fresh_blank_node_per_occurrence() {
    let src = b"<urn:s> <urn:p> [] , [] .";
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 256).unwrap(),
        TurtleTokenizer::default(),
    );
    let stmt = reader.next().unwrap().unwrap();
    let triples = stmt.triples();
    assert_eq!(triples.len(), 2);
    let (Term::Blank(BlankNode::Anonymous(a)), Term::Blank(BlankNode::Anonymous(b))) =
        (triples[0].object, triples[1].object)
    else {
        panic!("expected anonymous blank nodes")
    };
    assert_ne!(a, b);
}

#[test]
fn blank_node_property_list_as_subject_with_trailing_predicates() {
    let src = b"[ <urn:p> <urn:o> ] <urn:q> <urn:r> .";
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 256).unwrap(),
        TurtleTokenizer::default(),
    );
    let stmt = reader.next().unwrap().unwrap();
    let triples = stmt.triples();
    assert_eq!(triples.len(), 2);
    let Term::Blank(b1) = triples[0].subject else {
        panic!()
    };
    let Term::Blank(b2) = triples[1].subject else {
        panic!()
    };
    assert_eq!(b1, b2);
}

#[test]
fn collection_expands_to_rdf_list() {
    let src = b"<urn:s> <urn:p> ( <urn:a> <urn:b> ) .";
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 256).unwrap(),
        TurtleTokenizer::default(),
    );
    let stmt = reader.next().unwrap().unwrap();
    let triples = stmt.triples();
    // 2*2 (first/rest per element) + 1 (outer s p head, emitted last since
    // the collection's own triples are built before returning its head) = 5
    assert_eq!(triples.len(), 5);
    let Term::Iri(Iri::WellKnown(WellKnown::RdfFirst)) = triples[0].predicate else {
        panic!("expected rdf:first")
    };
    let Term::Iri(Iri::WellKnown(WellKnown::RdfRest)) = triples[1].predicate else {
        panic!("expected rdf:rest")
    };
    let Term::Iri(Iri::WellKnown(WellKnown::RdfNil)) = triples[3].object else {
        panic!("expected the list to terminate in rdf:nil")
    };
    let Term::Iri(Iri::Full(p)) = triples[4].predicate else {
        panic!("expected the outer <urn:p> triple last")
    };
    assert_eq!(p.text(stmt.bytes()), b"urn:p");
}

#[test]
fn empty_collection_is_rdf_nil_with_no_extra_triples() {
    let src = b"<urn:s> <urn:p> () .";
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 256).unwrap(),
        TurtleTokenizer::default(),
    );
    let stmt = reader.next().unwrap().unwrap();
    let triples = stmt.triples();
    assert_eq!(triples.len(), 1);
    let Term::Iri(Iri::WellKnown(WellKnown::RdfNil)) = triples[0].object else {
        panic!("expected rdf:nil")
    };
}

#[test]
fn literals_with_lang_and_datatype_and_numerics_and_booleans() {
    let src =
        b"<urn:s> <urn:p1> \"hello\"@en ; <urn:p2> \"5\"^^<urn:int> ; <urn:p3> 42 ; <urn:p4> true .";
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 256).unwrap(),
        TurtleTokenizer::default(),
    );
    let stmt = reader.next().unwrap().unwrap();
    let triples = stmt.triples();
    assert_eq!(triples.len(), 4);
    let Term::Literal(lit) = triples[0].object else {
        panic!()
    };
    assert!(matches!(lit.suffix, LiteralSuffix::Lang(_)));
    let Term::Literal(lit) = triples[1].object else {
        panic!()
    };
    assert!(matches!(lit.suffix, LiteralSuffix::Datatype(_)));
    assert!(matches!(triples[2].object, Term::Numeric(_, _)));
    assert!(matches!(triples[3].object, Term::Boolean(true)));
}

#[test]
fn statement_too_large_is_a_clear_error() {
    let mut src = String::from("<urn:s>");
    for i in 0..200 {
        src.push_str(&format!(" <urn:p{i}> <urn:o{i}> ;"));
    }
    src.push_str(" <urn:pLast> <urn:oLast> .");
    let mut reader = TurtleReader::new(
        SliceStream::new(src.as_bytes(), src.len() + 16).unwrap(),
        TurtleTokenizer::default(),
    );
    match reader.next() {
        Err(ParseError::Syntax { .. }) => {}
        other => panic!("expected an overflow syntax error, got {other:?}"),
    }
}

#[test]
fn window_boundaries_need_more_then_succeed() {
    let src =
        b"@prefix ex: <http://example.org/> .\nex:s ex:p \"hello world\"@en , [ ex:q ex:r ] .";
    for window in 1..=src.len() {
        let Ok(stream) = SliceStream::new(src, window) else {
            continue;
        };
        let mut reader = TurtleReader::new(stream, TurtleTokenizer::default());
        match reader.next() {
            Ok(Some(_)) => {}
            Err(ParseError::RecordTooLong { .. }) if window < src.len() => continue,
            other => panic!("window {window}: unexpected result {other:?}"),
        }
    }
}

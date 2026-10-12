use s_parser::{BlankId, NtWriter, ResolvedIri, ResolvedSuffix, ResolvedTerm};

fn nt(f: impl FnOnce(&mut NtWriter<&mut Vec<u8>>)) -> String {
    let mut buf = Vec::new();
    let mut w = NtWriter::new(&mut buf);
    f(&mut w);
    String::from_utf8(buf).unwrap()
}

#[test]
fn writes_a_plain_triple() {
    let out = nt(|w| {
        w.write_statement(
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:s")),
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:p")),
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:o")),
            None,
        )
        .unwrap();
    });
    assert_eq!(out, "<urn:s> <urn:p> <urn:o> .\n");
}

#[test]
fn joined_iri_is_written_without_a_seam() {
    let out = nt(|w| {
        w.write_statement(
            ResolvedTerm::Iri(ResolvedIri::Joined(b"http://example.org/", b"Thing")),
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:p")),
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:o")),
            None,
        )
        .unwrap();
    });
    assert_eq!(out, "<http://example.org/Thing> <urn:p> <urn:o> .\n");
}

#[test]
fn writes_a_quad_when_graph_is_some() {
    let out = nt(|w| {
        w.write_statement(
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:s")),
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:p")),
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:o")),
            Some(ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:g"))),
        )
        .unwrap();
    });
    assert_eq!(out, "<urn:s> <urn:p> <urn:o> <urn:g> .\n");
}

#[test]
fn literal_suffixes() {
    let out = nt(|w| {
        w.write_statement(
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:s")),
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:p")),
            ResolvedTerm::Literal {
                lexical: b"hello",
                suffix: ResolvedSuffix::Lang(b"en"),
            },
            None,
        )
        .unwrap();
        w.write_statement(
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:s")),
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:p")),
            ResolvedTerm::Literal {
                lexical: b"5",
                suffix: ResolvedSuffix::Datatype(ResolvedIri::Direct(
                    b"http://www.w3.org/2001/XMLSchema#integer",
                )),
            },
            None,
        )
        .unwrap();
        w.write_statement(
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:s")),
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:p")),
            ResolvedTerm::Literal {
                lexical: b"plain",
                suffix: ResolvedSuffix::None,
            },
            None,
        )
        .unwrap();
    });
    assert_eq!(
        out,
        "<urn:s> <urn:p> \"hello\"@en .\n\
         <urn:s> <urn:p> \"5\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n\
         <urn:s> <urn:p> \"plain\" .\n"
    );
}

#[test]
fn named_and_fresh_blank_ids_never_collide() {
    // `Fresh(5)` and a source label that happens to spell "5" must not
    // produce the same output label -- see `rdf::BlankId`'s doc comment.
    let out = nt(|w| {
        w.write_statement(
            ResolvedTerm::BlankNode(BlankId::Named(b"5")),
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:p")),
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:o")),
            None,
        )
        .unwrap();
        w.write_statement(
            ResolvedTerm::BlankNode(BlankId::Fresh(5)),
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:p")),
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:o")),
            None,
        )
        .unwrap();
    });
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2);
    let subject = |line: &str| line.split(' ').next().unwrap().to_string();
    assert_ne!(subject(lines[0]), subject(lines[1]));
    assert_eq!(subject(lines[0]), "_:l5");
    assert_eq!(subject(lines[1]), "_:b5");
}

#[test]
fn escapes_only_what_nt_forbids_raw() {
    // A lexical value already containing a validated ECHAR/UCHAR escape
    // passes through unchanged; a *raw* forbidden byte (as Turtle's long
    // strings allow) gets escaped.
    let out = nt(|w| {
        w.write_statement(
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:s")),
            ResolvedTerm::Iri(ResolvedIri::Direct(b"urn:p")),
            ResolvedTerm::Literal {
                lexical:
                    b"already \\n escaped, raw \" quote, raw \n newline, raw \r cr, \\u0041 uchar",
                suffix: ResolvedSuffix::None,
            },
            None,
        )
        .unwrap();
    });
    assert_eq!(
        out,
        "<urn:s> <urn:p> \"already \\n escaped, raw \\\" quote, raw \\n newline, raw \\r cr, \\u0041 uchar\" .\n"
    );
}

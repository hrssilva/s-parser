use s_parser::{NtTokenizer, NtWriter, QuadReader, SliceStream, TurtleReader, TurtleTokenizer};
use s_parser::{ntriples_to_ntriples, turtle_to_ntriples};

fn convert_turtle(src: &[u8]) -> String {
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 4096).unwrap(),
        TurtleTokenizer::default(),
    );
    let mut out = Vec::new();
    let mut writer = NtWriter::new(&mut out);
    turtle_to_ntriples(&mut reader, &mut writer).unwrap();
    String::from_utf8(out).unwrap()
}

fn convert_nt(src: &[u8]) -> String {
    let mut reader = QuadReader::new(SliceStream::new(src, 4096).unwrap(), NtTokenizer);
    let mut out = Vec::new();
    let mut writer = NtWriter::new(&mut out);
    ntriples_to_ntriples(&mut reader, &mut writer).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn plain_iris_pass_through() {
    let out = convert_turtle(b"<urn:s> <urn:p> <urn:o> .");
    assert_eq!(out, "<urn:s> <urn:p> <urn:o> .\n");
}

#[test]
fn prefixed_names_are_resolved_to_full_iris() {
    let out = convert_turtle(b"@prefix ex: <http://example.org/> .\nex:s ex:p ex:o .");
    assert_eq!(
        out,
        "<http://example.org/s> <http://example.org/p> <http://example.org/o> .\n"
    );
}

#[test]
fn a_keyword_becomes_rdf_type() {
    let out = convert_turtle(b"<urn:s> a <urn:Thing> .");
    assert_eq!(
        out,
        "<urn:s> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:Thing> .\n"
    );
}

#[test]
fn numeric_and_boolean_shorthand_are_canonicalized() {
    let out = convert_turtle(b"<urn:s> <urn:p1> 42 ; <urn:p2> 3.5 ; <urn:p3> true .");
    assert_eq!(
        out,
        "<urn:s> <urn:p1> \"42\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n\
         <urn:s> <urn:p2> \"3.5\"^^<http://www.w3.org/2001/XMLSchema#decimal> .\n\
         <urn:s> <urn:p3> \"true\"^^<http://www.w3.org/2001/XMLSchema#boolean> .\n"
    );
}

#[test]
fn collection_and_property_list_blank_nodes_stay_linked() {
    let out = convert_turtle(b"<urn:s> <urn:p> ( <urn:a> <urn:b> ) .");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 5);
    // <urn:s> <urn:p> _:bN .
    let head = lines[4].split(' ').nth(2).unwrap();
    // _:bN <rdf:first> <urn:a> .
    assert!(lines[0].starts_with(head));
    // _:bN <rdf:rest> _:bM .
    let rest = lines[1].split(' ').nth(2).unwrap();
    assert!(lines[2].starts_with(rest));
}

#[test]
fn long_string_with_raw_quote_and_newline_is_escaped_for_nt() {
    let out = convert_turtle(b"<urn:s> <urn:p> \"\"\"line one\nhas a \" quote\"\"\" .");
    assert_eq!(out, "<urn:s> <urn:p> \"line one\\nhas a \\\" quote\" .\n");
}

#[test]
fn single_quoted_string_with_raw_double_quote_is_escaped() {
    let out = convert_turtle(b"<urn:s> <urn:p> 'has a \" in it' .");
    assert_eq!(out, "<urn:s> <urn:p> \"has a \\\" in it\" .\n");
}

#[test]
fn ntriples_round_trips_through_conversion() {
    let src = b"<urn:s> <urn:p> \"hello\"@en .\n_:b0 <urn:p> <urn:o> .\n";
    let out = convert_nt(src);
    assert_eq!(
        out,
        "<urn:s> <urn:p> \"hello\"@en .\n_:lb0 <urn:p> <urn:o> .\n"
    );
}

#[test]
fn multiple_statements_stream_through_a_small_window() {
    let src = b"@prefix ex: <http://example.org/> .\nex:a ex:p ex:b .\nex:b ex:p ex:c .\nex:c ex:p ex:a .\n";
    let mut reader = TurtleReader::new(
        SliceStream::new(src, 48).unwrap(),
        TurtleTokenizer::default(),
    );
    let mut out = Vec::new();
    let mut writer = NtWriter::new(&mut out);
    turtle_to_ntriples(&mut reader, &mut writer).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert_eq!(out.lines().count(), 3);
    assert!(
        out.contains("<http://example.org/a> <http://example.org/p> <http://example.org/b> .\n")
    );
}

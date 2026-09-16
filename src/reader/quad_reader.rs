//! N-Triples' reader: a name for [`super::RecordReader`]/[`super::Record`]
//! specialized to the `[Token; 4]` shape [`crate::tokenizer::nt_tokenizer`]
//! produces, plus the ergonomic quad accessors that shape supports. There is
//! no separate reading mechanism here — see [`super::RecordReader`]'s doc
//! comment.
use super::{Record, RecordReader};
use crate::rdf::{Term, TermType, Token};

/// ```compile_fail
/// use s_parser::{QuadReader, SliceStream, NtTokenizer};
/// let mut reader = QuadReader::new(SliceStream::new(b"<s:a><p:b><o:c>.\n", 64).unwrap(), NtTokenizer);
/// let quad = reader.next().unwrap().unwrap();
/// reader.next().unwrap(); // Cannot release bytes still borrowed by quad.
/// println!("{:?}", quad.subject().text());
/// ```
pub type QuadReader<S, T> = RecordReader<S, T>;
pub type Quad<'a> = Record<'a, [Token; 4]>;

impl<'a> Quad<'a> {
    pub fn tokens(&self) -> &[Token; 4] {
        self.data()
    }
    pub fn subject(&self) -> Term<'a> {
        self.term(0)
    }
    pub fn predicate(&self) -> Term<'a> {
        self.term(1)
    }
    pub fn object(&self) -> Term<'a> {
        self.term(2)
    }
    pub fn graph(&self) -> Option<Term<'a>> {
        (self.tokens()[3].kind() != TermType::DefaultGraph).then(|| self.term(3))
    }
    fn term(&self, index: usize) -> Term<'a> {
        Term::new(self.tokens()[index], self.bytes())
    }
}

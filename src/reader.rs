pub mod parallel;
pub mod quad_reader;

/// A lending reader: returned records borrow this reader, so advancing cannot
/// invalidate live token text. Ordinary Iterator cannot express this lifetime.
pub trait Reader {
    type Record<'a>
    where
        Self: 'a;
    type Error;
    fn next(&mut self) -> Result<Option<Self::Record<'_>>, Self::Error>;
}

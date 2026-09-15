//! Variable-length lists (triple patterns, projected variables, function
//! arguments, ...) live in flat per-kind `Vec`s on [`super::algebra::Query`]
//! instead of nested `Vec`s or `Box<[_]>`s per node. A node stores a
//! `Range` — a start and length into the relevant pool — rather than owning
//! its own allocation. This keeps the algebra tree itself `Copy` throughout
//! and gives it one growable allocation per pool instead of one per list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Range {
    pub start: u32,
    pub len: u32,
}
impl Range {
    pub fn new(start: u32, len: u32) -> Self {
        Self { start, len }
    }
    pub fn indices(self) -> std::ops::Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }
    pub fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// An append-only pool with `Range`-addressed pushes. `push_slice` requires
/// contiguous placement (the common case: a list is built and pushed once),
/// which fails only if something else interleaved a push in between.
#[derive(Clone, Debug)]
pub(crate) struct Pool<T>(Vec<T>);
// A derived `Default` would add a spurious `T: Default` bound; `Vec<T>` is
// buildable empty for any `T`.
impl<T> Default for Pool<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}
impl<T> Pool<T> {
    pub fn get(&self, range: Range) -> &[T] {
        &self.0[range.indices()]
    }
    pub fn push(&mut self, value: T) -> u32 {
        let idx = self.0.len() as u32;
        self.0.push(value);
        idx
    }
    pub fn start(&self) -> u32 {
        self.0.len() as u32
    }
    pub fn range_from(&self, start: u32) -> Range {
        Range::new(start, self.0.len() as u32 - start)
    }
}
impl<T> std::ops::Index<usize> for Pool<T> {
    type Output = T;
    fn index(&self, i: usize) -> &T {
        &self.0[i]
    }
}
impl<T: Copy> Pool<T> {
    pub fn get_mut(&mut self, idx: u32) -> &mut T {
        &mut self.0[idx as usize]
    }
}

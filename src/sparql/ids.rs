//! Index types into the pools on [`super::algebra::Query`]. Kept as a
//! separate module so [`super::algebra`] and [`super::expr`] can each
//! reference the other's node kind by id without a cyclic `use`.
pub type NodeId = u32;
pub type ExprId = u32;
pub type PathId = u32;

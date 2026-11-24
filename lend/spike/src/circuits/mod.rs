//! Spike circuits. They reuse the lend program's note states and maths.
#[path = "../../../program/src/circuits/math.rs"]
pub mod math;
#[path = "../../../program/src/circuits/notes.rs"]
mod notes;
mod spike;

pub use notes::*;
pub use spike::*;

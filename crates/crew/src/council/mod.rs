//! A council: seats that each answer alone, talk until every seat agrees on every point, then
//! split the work and check it blind. A seat is a seat, not a model, and seats read one another
//! only through the relay.

pub mod checks;
pub mod deliver;
pub mod floor;
pub mod guards;
pub mod letters;
pub mod lexicon;
pub mod lift;
pub mod points;
pub mod private;
pub mod relay;
pub mod seats;
pub mod talk;
pub mod ways;

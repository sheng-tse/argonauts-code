//! What every handler shares: the context, what text means in each mode, the start mode,
//! notifications, and the test doubles.

pub(super) mod ctx;
pub(super) mod notify;
pub(super) mod route;
pub(super) mod start_mode;
#[cfg(test)]
pub(crate) mod test_support;

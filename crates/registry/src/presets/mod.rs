//! Presets: the built-ins, your own files in `~/.argonauts/presets/` and
//! `.argonauts/presets/`, and the built-ins written out as files you can read and edit.

mod builtin;
mod files;
mod list;
mod name;

pub(crate) use builtin::{DEFAULT_PRESET, builtin};
pub use files::{builtin_preset, upgrade_presets, write_preset};
pub use list::{presets, presets_in};
pub(crate) use name::{is_preset_name, not_a_preset_name};

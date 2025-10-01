pub mod fs;
pub mod hash;
pub mod parse;
pub mod streams;

use crate::registry::Registry;

/// Register all core contract implementations that are currently supported by the
/// Rust kernel runtime. This is intentionally granular so call-sites can opt-in
/// to specific domains (e.g. filesystem, http, ...).
pub fn register_core(registry: &Registry) {
    fs::register_fs(registry);
    hash::register_hash(registry);
    parse::register_parse(registry);
    streams::register_streams(registry);
}

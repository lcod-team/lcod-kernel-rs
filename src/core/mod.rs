pub mod array;
pub mod env;
pub mod fs;
pub mod git;
pub mod hash;
pub mod http;
pub mod json;
pub mod number;
pub mod object;
pub mod parse;
pub mod path;
pub mod runtime;
pub mod state;
pub mod streams;
pub mod string;
pub mod value;

use crate::registry::Registry;

/// Register all core contract implementations that are currently supported by the
/// Rust kernel runtime. This is intentionally granular so call-sites can opt-in
/// to specific domains (e.g. filesystem, http, ...).
pub fn register_core(registry: &Registry) {
    fs::register_fs(registry);
    env::register_env(registry);
    git::register_git(registry);
    hash::register_hash(registry);
    http::register_http(registry);
    parse::register_parse(registry);
    path::register_path(registry);
    streams::register_streams(registry);
    array::register_array(registry);
    object::register_object(registry);
    string::register_string(registry);
    json::register_json(registry);
    number::register_number(registry);
    value::register_value(registry);
    runtime::register_runtime(registry);
    state::register_state(registry);
}

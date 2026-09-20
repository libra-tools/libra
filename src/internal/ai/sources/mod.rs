//! KEEP source-resolution helpers used by hooks, sandbox, skills, and
//! commands. RC-23 deleted SourcePool / OpenAPI / config loaders
//! (`use tools` is forbidden here).

pub mod resolver;
pub mod security;

pub use resolver::{
    ConfigLayer, ConfigProvenance, ConfigResolveError, ResolvedConfig, ResolvedConfigDir,
    resolve_config_dir, resolve_config_file, surface_by_location,
};
pub use security::{
    format_resolve_error, format_security_parse_error, json_error_location,
    request_scope_for_workdir, resolve_security_dir, resolve_security_file, resolved_dir_paths,
    toml_error_location,
};

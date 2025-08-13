//! Internal implementation details - not part of public API

pub(crate) mod com_implementations;
pub(crate) mod module_loader;
pub(crate) mod plugin_impl;
pub(crate) mod utils;

pub(crate) mod isolated_plugin_impl;

#[cfg(target_os = "macos")]
pub(crate) mod objc_conflict_resolver;

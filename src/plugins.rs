//! Domain interfaces and adapters for Wasm plugins.
//!
//! Callers open persistent providers with explicit WASI capabilities and register
//! them with [`crate::Library`] or [`crate::Profiles`]. Catalog changes do not
//! replace existing sessions; callers open and register replacements explicitly.
//! Plugin futures require the caller's Tokio runtime with I/O and time enabled.

mod account;
mod library;

pub use account::{PluginAccountProvider, add_to_linker};
pub use library::PluginLibraryProvider;

mod interfaces {
    include!(concat!(env!("OUT_DIR"), "/plugin_interfaces.rs"));
}

pub use interfaces::PluginInterface;

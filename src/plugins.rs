//! Domain interfaces and adapters for Wasm plugins.

mod interfaces {
    include!(concat!(env!("OUT_DIR"), "/plugin_interfaces.rs"));
}

pub use interfaces::PluginInterface;

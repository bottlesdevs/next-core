use std::{collections::BTreeMap, env, fmt::Write, fs, path::PathBuf};

use heck::ToUpperCamelCase;
use wit_parser::{Resolve, WorldItem};

fn main() {
    let wit = "../next-plugin-api/wit";
    println!("cargo:rerun-if-changed={wit}");
    let mut resolve = Resolve::default();
    let (package, _) = resolve.push_dir(wit).unwrap();
    let mut interfaces = BTreeMap::new();
    for world in resolve.packages[package].worlds.values() {
        for (key, item) in &resolve.worlds[*world].exports {
            if let WorldItem::Interface { id, .. } = item {
                let name = resolve.interfaces[*id].name.as_ref().unwrap();
                interfaces.insert(resolve.name_world_key(key), name.to_upper_camel_case());
            }
        }
    }

    let mut source = String::from(
        "/// Known exported interfaces, generated from the SDK's WIT worlds.\n\
         #[derive(Clone, Copy, Debug, Eq, PartialEq)]\n\
         pub enum PluginInterface {\n",
    );
    for variant in interfaces.values() {
        writeln!(source, "    /// The {variant} interface.\n    {variant},").unwrap();
    }
    source.push_str("}\nimpl PluginInterface {\n    /// Returns the versioned WIT interface name.\n    pub const fn as_str(self) -> &'static str {\n        match self {\n");
    for (name, variant) in &interfaces {
        writeln!(source, "            Self::{variant} => {name:?},").unwrap();
    }
    source.push_str("        }\n    }\n}\nimpl AsRef<str> for PluginInterface {\n    fn as_ref(&self) -> &str {\n        (*self).as_str()\n    }\n}\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("plugin_interfaces.rs"),
        source,
    )
    .unwrap();
}

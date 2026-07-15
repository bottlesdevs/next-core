use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=proto/bottles.proto");
    println!("cargo:rerun-if-changed=proto/winebridge.proto");

    tonic_prost_build::configure()
        .type_attribute(
            ".",
            "#[derive(serde::Serialize, serde::Deserialize)] #[serde(rename_all = \"kebab-case\")]",
        )
        .compile_protos(
            &["proto/bottles.proto", "proto/winebridge.proto"],
            &["proto/"],
        )?;
    Ok(())
}

use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=proto/bottles.proto");
    println!("cargo:rerun-if-changed=proto/winebridge.proto");
    println!("cargo:rerun-if-changed=proto/fvs2d.proto");

    tonic_prost_build::configure().compile_protos(
        &[
            "proto/bottles.proto",
            "proto/winebridge.proto",
            "proto/fvs2d.proto",
        ],
        &["proto/"],
    )?;
    Ok(())
}

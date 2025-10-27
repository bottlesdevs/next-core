use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    tonic_prost_build::configure().compile_protos(
        &["proto/bottles.proto", "proto/winebridge.proto"],
        &["proto/"],
    )?;
    Ok(())
}

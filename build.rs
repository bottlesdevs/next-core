use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    tonic_build::compile_protos("proto/bottles.proto")?;
    tonic_build::compile_protos("proto/winebridge.proto")?;
    Ok(())
}

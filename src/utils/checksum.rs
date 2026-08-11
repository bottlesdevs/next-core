use std::{io, path::Path};

use futures_lite::io::AsyncReadExt;
use sha2::{Digest, Sha256, Sha512};

use crate::addons::Checksum;

pub(crate) async fn verify(path: &Path, checksum: &Checksum) -> io::Result<bool> {
    let mut file = async_fs::File::open(path).await?;
    let mut buffer = [0; 64 * 1024];
    let mut sha256 = Sha256::new();
    let mut sha512 = Sha512::new();
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        match checksum {
            Checksum::Sha256(_) => sha256.update(&buffer[..read]),
            Checksum::Sha512(_) => sha512.update(&buffer[..read]),
        }
    }
    let actual = match checksum {
        Checksum::Sha256(_) => format!("{:x}", sha256.finalize()),
        Checksum::Sha512(_) => format!("{:x}", sha512.finalize()),
    };
    Ok(actual == checksum.value())
}

use std::{fs::File, io::Write};

use serde::{Deserialize, Serialize};
use serde_json as json;

use tracing::info;

use crate::Error;

#[derive(Debug)]
pub struct WineBridge {
    pub wine_prefix: String,
    pub pipe: Option<File>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payload {
    pub action: WineBridgeAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WineBridgeAction {
    Run { executable: String },
}

impl WineBridge {
    pub fn new(wine_prefix: impl ToString) -> Self {
        Self {
            wine_prefix: wine_prefix.to_string(),
            pipe: None,
        }
    }

    pub fn connect(&mut self) -> Result<(), Error> {
        info!("Connecting to wine prefix: {}", self.wine_prefix);
        let pipe = unix_named_pipe::open_write(&self.wine_prefix)?;
        self.pipe = Some(pipe);
        Ok(())
    }

    pub fn send(&mut self, message: Payload) -> Result<(), Error> {
        info!("Sending message to wine prefix: {:#?}", message);
        let Some(pipe) = self.pipe.as_mut() else {
            return Err(Error::ConnectToBridgeError);
        };
        let payload = json::to_string(&message)? + "\n";
        pipe.write_all(payload.as_bytes())?;
        Ok(())
    }
}

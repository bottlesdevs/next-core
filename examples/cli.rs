use bottles_core::{Error, Payload, WineBridge, WineBridgeAction};
use tracing::Level;

pub fn main() -> Result<(), Error> {
    let wine_prefix = std::env::args()
        .nth(1)
        .expect("Wine prefix path is required");
    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .init();
    let mut bridge = WineBridge::new(wine_prefix);
    bridge.connect()?;
    bridge.send(Payload {
        action: WineBridgeAction::Run {
            executable: "explorer.exe".to_string(),
        },
    })
}

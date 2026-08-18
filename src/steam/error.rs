#[derive(Debug, thiserror::Error)]
pub enum SteamError {
    #[error("Steam account {steam_id64} is already linked to profile \"{linked_profile_name}\"")]
    SteamAccountAlreadyLinked {
        steam_id64: String,
        linked_profile_name: String,
    },
}

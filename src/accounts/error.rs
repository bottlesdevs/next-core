use next_proto::bottles::common::v1::Storefront;

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("invalid storefront: {0}")]
    InvalidStorefront(Storefront),
}

use serde::{de::DeserializeOwned, Serialize};

pub mod client;
pub mod server;

pub trait UdsRequest: Serialize + DeserializeOwned + Send + Sync + 'static {}

pub trait UdsResponse: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// Create a response representing that the request could not be parsed.
    fn request_parse_error_response() -> Self;

    /// Create a response representing an internal transport error.
    fn internal_error_response(msg: String) -> Self;
}

// TODO: Test that the client and server can communicate with each other.

use super::{UdsRequest, UdsResponse};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

#[derive(Clone)]
pub struct UnixDomainSocketClientTransport {
    uds_address: String,
}

impl UnixDomainSocketClientTransport {
    pub fn new(uds_address: String) -> Self {
        Self { uds_address }
    }

    pub async fn send_request<Request: UdsRequest, Response: UdsResponse>(
        &self,
        request: Request,
    ) -> Result<Response, UdsClientError> {
        let serialized_request =
            serde_json::to_vec(&request).map_err(|_| UdsClientError::RequestSerializationError)?;
        let serialize_response = self.send_and_receive_bytes(serialized_request).await?;
        serde_json::from_slice::<Response>(&serialize_response)
            .map_err(|_| UdsClientError::MalformedResponse)
    }

    async fn send_and_receive_bytes(
        &self,
        serialized_request: Vec<u8>,
    ) -> Result<Vec<u8>, UdsClientError> {
        // Open up a UDS connection to the server.
        let mut socket = UnixStream::connect(&self.uds_address)
            .await
            .map_err(|_| UdsClientError::ServerNotRunning)?;

        socket
            .write_all(&serialized_request)
            .await
            .map_err(|_| UdsClientError::UdsSocketError)?;
        socket
            .shutdown()
            .await
            .map_err(|_| UdsClientError::UdsSocketError)?;

        // Read the response from the server.
        // TODO: Add a timeout to this read operation.
        let mut buf = Vec::new();
        socket
            .read_to_end(&mut buf)
            .await
            .map_err(|_| UdsClientError::UdsSocketError)?;
        Ok(buf)
    }
}

/// Errors that can occur when communicating with a Unix domain socket server.
#[derive(Clone, Debug, PartialEq)]
pub enum UdsClientError {
    /// A Unix domain socket server is not running on the specified address.
    ServerNotRunning,

    /// An I/O error occurred while writing to or reading from the Unix domain socket.
    UdsSocketError,

    /// An error occurred while serializing the request.
    RequestSerializationError,

    /// Received a response from the server that that cannot be parsed.
    MalformedResponse,
}

impl std::fmt::Display for UdsClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UdsClientError::ServerNotRunning => {
                write!(f, "Unix domain socket server not running.")
            }
            UdsClientError::UdsSocketError => {
                write!(f, "Error writing to or reading from Unix domain socket.")
            }
            UdsClientError::RequestSerializationError => {
                write!(f, "Error serializing the request.")
            }
            UdsClientError::MalformedResponse => {
                write!(
                    f,
                    "Received a response from the server that that cannot be parsed."
                )
            }
        }
    }
}

impl std::error::Error for UdsClientError {}

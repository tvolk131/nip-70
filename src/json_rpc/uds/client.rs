use super::super::{JsonRpcClientTransport, JsonRpcRequest, JsonRpcResponse};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

#[derive(Clone)]
pub struct UnixDomainSocketJsonRpcClientTransport {
    uds_address: String,
}

impl UnixDomainSocketJsonRpcClientTransport {
    pub fn new(uds_address: String) -> Self {
        Self { uds_address }
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

impl JsonRpcClientTransport<UdsClientError> for UnixDomainSocketJsonRpcClientTransport {
    async fn send_request(
        &self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, UdsClientError> {
        let serialized_request =
            serde_json::to_vec(&request).expect("Failed to serialize JSON-RPC request.");
        let serialize_response = self.send_and_receive_bytes(serialized_request).await?;
        serde_json::from_slice::<JsonRpcResponse>(&serialize_response)
            .map_err(|_| UdsClientError::MalformedResponse)
    }

    async fn send_batch_request(
        &self,
        requests: Vec<JsonRpcRequest>,
    ) -> Result<Vec<JsonRpcResponse>, UdsClientError> {
        let serialized_requests =
            serde_json::to_vec(&requests).expect("Failed to serialize JSON-RPC batch request.");
        let serialize_responses = self.send_and_receive_bytes(serialized_requests).await?;
        serde_json::from_slice::<Vec<JsonRpcResponse>>(&serialize_responses)
            .map_err(|_| UdsClientError::MalformedResponse)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum UdsClientError {
    /// A Unix domain socket server is not running on the specified address.
    ServerNotRunning,

    /// An I/O error occurred while writing to or reading from the Unix domain socket.
    UdsSocketError,

    /// Received a response from the server that does not conform to the JSON-RPC 2.0 protocol.
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
            UdsClientError::MalformedResponse => {
                write!(
                    f,
                    "Received a response from the server that does not conform to the JSON-RPC 2.0 protocol."
                )
            }
        }
    }
}

impl std::error::Error for UdsClientError {}

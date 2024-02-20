use crate::json_rpc::JsonRpcServerTransport;
use crate::{JsonRpcRequest, JsonRpcResponse};
use futures::SinkExt;
use std::path::Path;
use std::pin::Pin;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

use super::{JsonRpcClientTransport, JsonRpcErrorCode};

pub struct UnixDomainSocketJsonRpcServerTransport {
    uds_task_handle: tokio::task::JoinHandle<()>,
    rpc_receiver: futures::channel::mpsc::Receiver<(
        JsonRpcRequest,
        futures::channel::oneshot::Sender<JsonRpcResponse>,
    )>,
    uds_address: String,
}

impl std::ops::Drop for UnixDomainSocketJsonRpcServerTransport {
    fn drop(&mut self) {
        // Abort the UDS task, since it will loop forever otherwise.
        self.uds_task_handle.abort();

        // Try to remove the UDS file. If it fails, it's not a big deal.
        let _ = std::fs::remove_file(&self.uds_address);
    }
}

impl UnixDomainSocketJsonRpcServerTransport {
    pub fn connect_and_start(uds_address: String, buffer_size: usize) -> std::io::Result<Self> {
        if Path::new(&uds_address).exists() {
            std::fs::remove_file(&uds_address)?;
        }

        let (rpc_sender, rpc_receiver) = futures::channel::mpsc::channel(1024);

        let listener = UnixListener::bind(&uds_address)?;

        let uds_task_handle = tokio::spawn(async move {
            loop {
                let mut rpc_sender_clone = rpc_sender.clone();

                if let Ok((mut socket, _)) = listener.accept().await {
                    // TODO: Grab the task handle and cancel it when the server is dropped.
                    let handle: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
                        let mut buf = vec![0; buffer_size];
                        let mut rolling_buf: Vec<u8> = Vec::new();
                        let request: JsonRpcRequest;

                        // TODO: Add a timeout to this read operation.
                        loop {
                            socket.readable().await?;
                            if let Ok(nbytes) = socket.try_read(&mut buf) {
                                for byte in &buf[..nbytes] {
                                    rolling_buf.push(*byte);
                                }
                            } else if let Ok(parsed_request) =
                                serde_json::from_slice::<JsonRpcRequest>(&rolling_buf)
                            {
                                request = parsed_request;
                                break;
                            }
                        }

                        let (tx, rx) = futures::channel::oneshot::channel();
                        let request_id = request.id();
                        rpc_sender_clone.send((request, tx)).await.unwrap();
                        let response = match rx.await {
                            Ok(response) => response,
                            Err(_) => JsonRpcResponse::new(
                                super::JsonRpcResponseData::Error {
                                    error: super::JsonRpcError {
                                        code: JsonRpcErrorCode::InternalError,
                                        message: "Internal error: Response sender dropped"
                                            .to_string(),
                                        data: None,
                                    },
                                },
                                request_id,
                            ),
                        };
                        let serialized_response = serde_json::to_vec(&response)?;

                        socket.writable().await?;
                        socket.write_all(&serialized_response).await?;
                        socket.shutdown().await?;

                        Ok(())
                    });
                }
            }
        });

        Ok(Self {
            uds_task_handle,
            rpc_receiver,
            uds_address,
        })
    }

    fn project(
        self: Pin<&mut Self>,
    ) -> Pin<
        &mut futures::channel::mpsc::Receiver<(
            JsonRpcRequest,
            futures::channel::oneshot::Sender<JsonRpcResponse>,
        )>,
    > {
        unsafe { self.map_unchecked_mut(|x| &mut x.rpc_receiver) }
    }
}

impl futures::Stream for UnixDomainSocketJsonRpcServerTransport {
    type Item = (
        JsonRpcRequest,
        futures::channel::oneshot::Sender<JsonRpcResponse>,
    );

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.project().poll_next(cx)
    }
}

impl JsonRpcServerTransport for UnixDomainSocketJsonRpcServerTransport {}

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

        let mut bytes_written = 0;
        while bytes_written < serialized_request.len() {
            socket
                .writable()
                .await
                .map_err(|_| UdsClientError::UdsSocketError)?;
            bytes_written += socket
                .try_write(&serialized_request[bytes_written..])
                .unwrap_or(0);
        }
        socket
            .flush()
            .await
            .map_err(|_| UdsClientError::UdsSocketError)?;

        // Read the response from the server.
        // TODO: Add a timeout to this read operation.
        socket
            .readable()
            .await
            .map_err(|_| UdsClientError::UdsSocketError)?;
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
        Ok(
            serde_json::from_slice::<JsonRpcResponse>(&serialize_response)
                .map_err(|_| UdsClientError::MalformedResponse)?,
        )
    }

    async fn send_batch_request(
        &self,
        requests: Vec<JsonRpcRequest>,
    ) -> Result<Vec<JsonRpcResponse>, UdsClientError> {
        let serialized_requests =
            serde_json::to_vec(&requests).expect("Failed to serialize JSON-RPC batch request.");
        let serialize_responses = self.send_and_receive_bytes(serialized_requests).await?;
        Ok(
            serde_json::from_slice::<Vec<JsonRpcResponse>>(&serialize_responses)
                .map_err(|_| UdsClientError::MalformedResponse)?,
        )
    }
}

#[derive(Debug, PartialEq)]
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

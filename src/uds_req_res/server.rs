use futures::SinkExt;
use std::path::Path;
use std::pin::Pin;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::json_rpc::{JsonRpcRequest, JsonRpcResponse, JsonRpcServerTransport};

use super::{UdsRequest, UdsResponse};

pub struct UnixDomainSocketServerTransport<Request: UdsRequest, Response: UdsResponse> {
    uds_task_handle: tokio::task::JoinHandle<()>,
    rpc_receiver:
        futures::channel::mpsc::Receiver<(Request, futures::channel::oneshot::Sender<Response>)>,
    uds_address: String,
}

impl JsonRpcServerTransport for UnixDomainSocketServerTransport<JsonRpcRequest, JsonRpcResponse> {}

impl<Request: UdsRequest, Response: UdsResponse> std::ops::Drop
    for UnixDomainSocketServerTransport<Request, Response>
{
    fn drop(&mut self) {
        // Abort the UDS task, since it will loop forever otherwise.
        self.uds_task_handle.abort();

        // Try to remove the UDS file. If it fails, it's not a big deal.
        let _ = std::fs::remove_file(&self.uds_address);
    }
}

impl<Request: UdsRequest, Response: UdsResponse>
    UnixDomainSocketServerTransport<Request, Response>
{
    /// Create a new `UnixDomainSocketServerTransport` and start listening for incoming
    /// connections. **MUST** be called from within a tokio runtime.
    pub fn connect_and_start(uds_address: String) -> std::io::Result<Self> {
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
                    tokio::spawn(async move {
                        let mut buf = Vec::new();
                        socket.read_to_end(&mut buf).await?;
                        let request = match serde_json::from_slice::<Request>(&buf) {
                            Ok(request) => request,
                            Err(_) => {
                                return Self::send_response_to_socket(
                                    socket,
                                    Response::request_parse_error_response(),
                                )
                                .await;
                            }
                        };

                        let (tx, rx) = futures::channel::oneshot::channel();
                        // TODO: Remove this unwrap. For now it's safe because the receiver will only be dropped when the server is dropped.
                        rpc_sender_clone.send((request, tx)).await.unwrap();
                        let response = match rx.await {
                            Ok(response) => response,
                            Err(_) => Response::internal_error_response(
                                "Internal error: Response sender dropped".to_string(),
                            ),
                        };

                        Self::send_response_to_socket(socket, response).await
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

    /// Send a JSON-RPC response to the client that sent the request.
    /// Intentionally consumes the `UnixStream` to prevent the caller
    /// from sending multiple responses to the same request.
    async fn send_response_to_socket(
        mut socket: UnixStream,
        response: Response,
    ) -> Result<(), std::io::Error> {
        let serialized_response = serde_json::to_vec(&response)?;
        socket.write_all(&serialized_response).await?;
        socket.shutdown().await?;
        Ok(())
    }

    fn project(
        self: Pin<&mut Self>,
    ) -> Pin<
        &mut futures::channel::mpsc::Receiver<(
            Request,
            futures::channel::oneshot::Sender<Response>,
        )>,
    > {
        unsafe { self.map_unchecked_mut(|x| &mut x.rpc_receiver) }
    }
}

impl<Request: UdsRequest, Response: UdsResponse> futures::Stream
    for UnixDomainSocketServerTransport<Request, Response>
{
    type Item = (Request, futures::channel::oneshot::Sender<Response>);

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.project().poll_next(cx)
    }
}

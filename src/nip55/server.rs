use super::nip04_jsonrpc::{
    jsonrpc_response_to_nip04_encrypted_event, nip04_encrypted_event_to_jsonrpc_request,
};
use crate::uds_req_res::UdsResponse;
use crate::{
    json_rpc::{JsonRpcRequest, JsonRpcResponse, JsonRpcServerTransport},
    uds_req_res::server::UnixDomainSocketServerTransport,
};
use futures::{FutureExt, StreamExt};
use nostr_sdk::{Event, Keys};
use std::task::Poll;

pub struct UnixDomainSocketNip04Server {
    transport_server: UnixDomainSocketServerTransport<Event, Event>,
    server_keypair: Keys,
}

impl UnixDomainSocketNip04Server {
    /// Create a new `UnixDomainSocketNip04Server` and start listening for incoming
    /// connections. **MUST** be called from within a tokio runtime.
    pub fn connect_and_start(uds_address: String, server_keypair: Keys) -> std::io::Result<Self> {
        Ok(Self {
            transport_server: UnixDomainSocketServerTransport::connect_and_start(uds_address)?,
            server_keypair,
        })
    }
}

impl futures::Stream for UnixDomainSocketNip04Server {
    type Item = (
        JsonRpcRequest,
        futures::channel::oneshot::Sender<JsonRpcResponse>,
    );

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let (request_event, response_event_sender) = match self.transport_server.poll_next_unpin(cx)
        {
            Poll::Ready(Some((request_event, response_event_sender))) => {
                (request_event, response_event_sender)
            }
            Poll::Ready(None) => return Poll::Pending,
            Poll::Pending => return Poll::Pending,
        };

        let request_event_kind = request_event.kind();
        let request_event_author = request_event.author();

        let request =
            match nip04_encrypted_event_to_jsonrpc_request(&request_event, &self.server_keypair) {
                Ok(request) => request,
                Err(_) => return Poll::Pending,
            };

        let (response_sender, response_receiver) = futures::channel::oneshot::channel();

        let server_keypair = self.server_keypair.clone();
        tokio::spawn(async move {
            response_receiver
                .then(|response| async {
                    match response {
                        Ok(response) => {
                            let response_event = jsonrpc_response_to_nip04_encrypted_event(
                                request_event_kind,
                                &response,
                                request_event_author,
                                &server_keypair,
                            )
                            .unwrap();
                            response_event_sender.send(response_event).unwrap();
                        }
                        Err(_) => {
                            let response_event = jsonrpc_response_to_nip04_encrypted_event(
                                request_event_kind,
                                &JsonRpcResponse::internal_error_response(
                                    "Internal error.".to_string(),
                                ),
                                request_event_author,
                                &server_keypair,
                            )
                            .unwrap();
                            response_event_sender.send(response_event).unwrap();
                        }
                    }
                })
                .await;
        });

        Poll::Ready(Some((request, response_sender)))
    }
}

impl JsonRpcServerTransport for UnixDomainSocketNip04Server {}

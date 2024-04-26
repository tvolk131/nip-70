use crate::uds_req_res::server::UnixDomainSocketServerTransport;

use super::super::super::json_rpc::JsonRpcServerTransport;
use super::super::{JsonRpcRequest, JsonRpcResponse};
use futures::StreamExt;

pub struct UnixDomainSocketJsonRpcServerTransport {
    transport_server: UnixDomainSocketServerTransport<JsonRpcRequest, JsonRpcResponse>,
}

impl UnixDomainSocketJsonRpcServerTransport {
    /// Create a new `UnixDomainSocketJsonRpcServerTransport` and start listening for incoming
    /// connections. **MUST** be called from within a tokio runtime.
    pub fn connect_and_start(uds_address: String) -> std::io::Result<Self> {
        Ok(Self {
            transport_server: UnixDomainSocketServerTransport::connect_and_start(uds_address)?,
        })
    }
}

impl futures::Stream for UnixDomainSocketJsonRpcServerTransport {
    type Item = (
        JsonRpcRequest,
        futures::channel::oneshot::Sender<JsonRpcResponse>,
    );

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.transport_server.poll_next_unpin(cx)
    }
}

impl JsonRpcServerTransport for UnixDomainSocketJsonRpcServerTransport {}

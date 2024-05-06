use super::{
    super::uds_req_res::client::{UdsClientError, UnixDomainSocketClientTransport},
    nip04_jsonrpc::{
        jsonrpc_request_to_nip04_encrypted_event, nip04_encrypted_event_to_jsonrpc_response,
    },
};
use crate::json_rpc::{JsonRpcRequest, JsonRpcResponse};
use nostr_sdk::{Event, Keys, Kind, PublicKey};

#[derive(Clone)]
pub struct UnixDomainSocketNip46Client {
    uds_client_transport: UnixDomainSocketClientTransport,
}

// TODO: Support batch requests.
impl UnixDomainSocketNip46Client {
    pub fn new(uds_address: String) -> Self {
        Self {
            uds_client_transport: UnixDomainSocketClientTransport::new(uds_address),
        }
    }

    pub async fn send_request(
        &self,
        kind: Kind,
        request: &JsonRpcRequest,
        server_pubkey: PublicKey,
    ) -> Result<JsonRpcResponse, UdsClientError> {
        let temp_client_keypair = Keys::generate();

        let request_event: Event = jsonrpc_request_to_nip04_encrypted_event(
            kind,
            request,
            &temp_client_keypair,
            server_pubkey,
        )
        .map_err(|_| UdsClientError::RequestSerializationError)?;

        let response_event = self
            .uds_client_transport
            .send_request(request_event)
            .await?;

        let response: JsonRpcResponse =
            nip04_encrypted_event_to_jsonrpc_response(&response_event, &temp_client_keypair)
                .map_err(|_| UdsClientError::MalformedResponse)?;

        Ok(response)
    }
}

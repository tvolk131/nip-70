use async_trait::async_trait;
use json_rpc::{
    JsonRpcError, JsonRpcErrorCode, JsonRpcId, JsonRpcRequest, JsonRpcResponseData, JsonRpcServer,
    JsonRpcServerHandler, JsonRpcStructuredValue,
};
use nip04_over_uds::{client::UnixDomainSocketNip46Client, server::UnixDomainSocketNip04Server};
use nostr_sdk::{Event, Keys, Kind, PublicKey, UnsignedEvent};
use serde::{Deserialize, Serialize};
use serde_json::json;
mod nip04_over_uds;
use std::sync::Arc;
pub use uds_req_res::client::UdsClientError;

mod json_rpc;
mod uds_req_res;

const NIP70_UDS_ADDRESS: &str = "/tmp/nip-70.sock";

const METHOD_NAME_SIGN_EVENT: &str = "signEvent";

/// Errors that can be returned from [`Nip70`] trait functions.
#[derive(Clone, Debug, PartialEq)]
pub enum Nip70ServerError {
    /// The server rejected the request. This most likely means that the user
    /// declined to perform the operation for the app that requested it.
    Rejected,

    /// The server encountered an internal error while processing the request.
    InternalError,

    /// The server does not support the requested method.
    MethodNotFound,
}

impl Nip70ServerError {
    fn to_json_rpc_error(&self) -> JsonRpcError {
        match self {
            Nip70ServerError::Rejected => {
                JsonRpcError::new(JsonRpcErrorCode::Custom(1), "Rejected".to_string(), None)
            }
            Nip70ServerError::InternalError => JsonRpcError::new(
                JsonRpcErrorCode::InternalError,
                "Internal error".to_string(),
                None,
            ),
            Nip70ServerError::MethodNotFound => JsonRpcError::new(
                JsonRpcErrorCode::MethodNotFound,
                "Method not found".to_string(),
                None,
            ),
        }
    }
}

/// Defines the server-side functionality for the NIP-70 protocol.
/// Implement this trait and pass it to [`Nip70Server::start()`] to run a NIP-70 server.
#[async_trait]
pub trait Nip70: Send + Sync {
    /// Signs a Nostr event on behalf of the signed-in user.
    async fn sign_event(&self, event: UnsignedEvent) -> Result<Event, Nip70ServerError>;
}

/// A server for the NIP-70 protocol.
pub struct Nip70Server {
    json_rpc_server: JsonRpcServer,
}

impl Nip70Server {
    /// Creates and starts a NIP-70 compliant Unix domain socket server.
    pub fn start(nip70: Arc<dyn Nip70>, server_keypair: Keys) -> std::io::Result<Self> {
        Self::start_internal(nip70, NIP70_UDS_ADDRESS.to_string(), server_keypair)
    }

    fn start_internal(
        nip70: Arc<dyn Nip70>,
        uds_address: String,
        server_keypair: Keys,
    ) -> std::io::Result<Self> {
        Ok(Self {
            json_rpc_server: JsonRpcServer::new(
                Box::from(UnixDomainSocketNip04Server::connect_and_start(
                    uds_address,
                    server_keypair,
                )?),
                Box::from(Nip70ServerHandler { nip70 }),
            ),
        })
    }

    /// Stops the NIP-70 server.
    pub fn stop(self) {
        self.json_rpc_server.stop();
    }
}

struct Nip70ServerHandler {
    nip70: Arc<dyn Nip70>,
}

#[async_trait::async_trait]
impl JsonRpcServerHandler for Nip70ServerHandler {
    async fn handle_batch_request(
        &self,
        requests: Vec<JsonRpcRequest>,
    ) -> Vec<JsonRpcResponseData> {
        let mut responses = Vec::new();

        for request in requests {
            let parsed_request = match Nip70Request::from_json_rpc_request(&request) {
                Ok(request) => request,
                Err(error) => {
                    responses.push(JsonRpcResponseData::Error {
                        error: error.to_json_rpc_error(),
                    });
                    continue;
                }
            };

            let response_or = match parsed_request {
                // TODO: Let's get the pubkey and check it against the unsigned event before signing.
                Nip70Request::SignEvent(event) => match self.nip70.sign_event(event).await {
                    Ok(event) => Ok(Nip70Response::SignEvent(event)),
                    Err(err) => Err(err),
                },
            };

            responses.push(match response_or {
                Ok(response) => response.to_json_rpc_response_data(),
                Err(err) => JsonRpcResponseData::Error {
                    error: err.to_json_rpc_error(),
                },
            });
        }

        responses
    }
}

/// Errors that can be returned from [`Nip70Client`] functions.
#[derive(Clone, Debug, PartialEq)]
pub enum Nip70ClientError {
    UdsClientError(UdsClientError),
    ProtocolError,
    ServerError(Nip70ServerError),
}

impl Nip70ClientError {
    fn from_json_rpc_error(error: &JsonRpcError) -> Self {
        match error.code() {
            JsonRpcErrorCode::Custom(1) => Self::ServerError(Nip70ServerError::Rejected),
            JsonRpcErrorCode::InternalError => Self::ServerError(Nip70ServerError::InternalError),
            JsonRpcErrorCode::MethodNotFound => Self::ServerError(Nip70ServerError::MethodNotFound),
            _ => Self::ProtocolError,
        }
    }
}

/// A client for the NIP-70 protocol.
#[derive(Clone)]
pub struct Nip70Client {
    transport: UnixDomainSocketNip46Client,
}

impl Default for Nip70Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Nip70Client {
    pub fn new() -> Self {
        Self::new_internal(NIP70_UDS_ADDRESS.to_string())
    }

    fn new_internal(uds_address: String) -> Self {
        Self {
            transport: UnixDomainSocketNip46Client::new(uds_address),
        }
    }

    /// Signs a Nostr event on behalf of the signed-in user using the NIP-70 server.
    pub async fn sign_event(
        &self,
        event: UnsignedEvent,
        server_pubkey: PublicKey,
    ) -> Result<Event, Nip70ClientError> {
        self.send_request(Nip70Request::SignEvent(event), server_pubkey)
            .await
            .map(|response| match response {
                Nip70Response::SignEvent(event) => Ok(event),
            })?
    }

    async fn send_request(
        &self,
        request: Nip70Request,
        server_pubkey: PublicKey,
    ) -> Result<Nip70Response, Nip70ClientError> {
        // TODO: Use a real request id.
        let json_rpc_request = request.to_json_rpc_request(JsonRpcId::Null);
        let json_rpc_response = self
            .transport
            .send_request(Kind::NostrConnect, &json_rpc_request, server_pubkey)
            .await
            .map_err(Nip70ClientError::UdsClientError)?;
        Nip70Response::from_json_rpc_response_data(json_rpc_response.data())
    }
}

enum Nip70Request {
    SignEvent(UnsignedEvent),
}

impl Nip70Request {
    fn get_method_name(&self) -> &str {
        match self {
            Nip70Request::SignEvent(_) => METHOD_NAME_SIGN_EVENT,
        }
    }

    fn get_params(&self) -> Option<JsonRpcStructuredValue> {
        match self {
            Nip70Request::SignEvent(event) => Some(JsonRpcStructuredValue::Object(
                // This should never panic, since we're converting an `UnsignedEvent`
                // struct, which should always serialize to a JSON object.
                json!(event)
                    .as_object()
                    .expect("Failed to convert event to object")
                    .clone(),
            )),
        }
    }

    fn to_json_rpc_request(&self, request_id: JsonRpcId) -> JsonRpcRequest {
        JsonRpcRequest::new(
            self.get_method_name().to_string(),
            self.get_params(),
            request_id,
        )
    }

    fn from_json_rpc_request(request: &JsonRpcRequest) -> Result<Self, Nip70ServerError> {
        match request.method() {
            METHOD_NAME_SIGN_EVENT => Ok(Nip70Request::SignEvent(
                if let Ok(value) =
                    serde_json::from_value(match request.params().map(|v| v.clone().into_value()) {
                        Some(value) => value,
                        None => return Err(Nip70ServerError::InternalError),
                    })
                {
                    value
                } else {
                    return Err(Nip70ServerError::InternalError);
                },
            )),
            _ => Err(Nip70ServerError::MethodNotFound),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Nip70Response {
    SignEvent(Event),
}

impl Nip70Response {
    fn to_json_rpc_response_data(&self) -> JsonRpcResponseData {
        JsonRpcResponseData::Success {
            result: serde_json::to_value(self).unwrap(),
        }
    }

    fn from_json_rpc_response_data(
        response: &JsonRpcResponseData,
    ) -> Result<Self, Nip70ClientError> {
        let result = match response {
            JsonRpcResponseData::Success { result } => result,
            JsonRpcResponseData::Error { error } => {
                return Err(Nip70ClientError::from_json_rpc_error(error))
            }
        };

        if let Ok(value) = serde_json::from_value(result.clone()) {
            Ok(value)
        } else {
            Err(Nip70ClientError::ProtocolError)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use nostr_sdk::{EventId, Keys, Kind, Timestamp};
    use std::{sync::Mutex, time::Duration};

    struct TestNip70Implementation {
        keys: Keys,
        reject_all_requests: bool,
    }

    impl TestNip70Implementation {
        fn new_with_generated_keys() -> (Self, Keys) {
            let keys = Keys::generate();

            (
                Self {
                    keys: keys.clone(),
                    reject_all_requests: false,
                },
                keys,
            )
        }

        fn new_rejecting_all_requests() -> (Self, Keys) {
            let keys = Keys::generate();

            (
                Self {
                    keys: keys.clone(),
                    reject_all_requests: true,
                },
                keys,
            )
        }
    }

    #[async_trait]
    impl Nip70 for TestNip70Implementation {
        async fn sign_event(&self, event: UnsignedEvent) -> Result<Event, Nip70ServerError> {
            if self.reject_all_requests {
                return Err(Nip70ServerError::Rejected);
            }

            event
                .sign(&self.keys)
                .map_err(|_| Nip70ServerError::InternalError)
        }
    }

    lazy_static::lazy_static! {
        static ref UDS_ADDRESS_COUNTER: Arc<Mutex<i32>> = Arc::new(Mutex::new(0));
    }

    fn get_free_uds_address() -> String {
        let mut counter = UDS_ADDRESS_COUNTER.lock().unwrap();
        let uds_address = format!("/tmp/nip70-{}.sock", *counter);
        *counter += 1;
        uds_address
    }

    fn get_nip70_server_and_client_test_pair(
        nip70: Arc<dyn Nip70>,
    ) -> (Nip70Server, Nip70Client, Keys) {
        let uds_address = get_free_uds_address();
        let server_keypair = Keys::generate();
        let server =
            Nip70Server::start_internal(nip70, uds_address.clone(), server_keypair.clone())
                .unwrap();
        let client = Nip70Client::new_internal(uds_address);
        (server, client, server_keypair)
    }

    #[tokio::test]
    async fn sign_event_over_uds() {
        let (nip70, keys) = TestNip70Implementation::new_with_generated_keys();
        let (server, client, server_keypair) =
            get_nip70_server_and_client_test_pair(Arc::from(nip70));

        let pubkey = keys.public_key();
        let created_at = Timestamp::now();
        let kind = Kind::TextNote;
        let tags = vec![];
        let content = String::from("Hello, world!");
        let unsigned_event = UnsignedEvent {
            id: EventId::new(&pubkey, created_at, &kind, &tags, &content),
            pubkey,
            created_at,
            kind,
            tags,
            content,
        };

        let event = client
            .sign_event(unsigned_event, server_keypair.public_key())
            .await
            .unwrap();

        assert!(event.verify().is_ok());

        server.stop();
    }

    #[tokio::test]
    async fn sign_large_event_over_uds() {
        let (nip70, keys) = TestNip70Implementation::new_with_generated_keys();
        let (server, client, server_keypair) =
            get_nip70_server_and_client_test_pair(Arc::from(nip70));

        let pubkey = keys.public_key();
        let created_at = Timestamp::now();
        let kind = Kind::TextNote;
        let tags = vec![];
        let content: String = std::iter::repeat('a').take((2 as usize).pow(20)).collect();
        let unsigned_event = UnsignedEvent {
            id: EventId::new(&pubkey, created_at, &kind, &tags, &content),
            pubkey,
            created_at,
            kind,
            tags,
            content,
        };

        let event = client
            .sign_event(unsigned_event, server_keypair.public_key())
            .await
            .unwrap();

        assert!(event.verify().is_ok());

        server.stop();
    }

    #[test]
    #[should_panic(expected = "must be called from the context of a Tokio 1.x runtime")]
    fn run_server_without_async_runtime() {
        let uds_address = get_free_uds_address();
        let (nip70, keys) = TestNip70Implementation::new_with_generated_keys();
        Nip70Server::start_internal(Arc::from(nip70), uds_address.clone(), keys).unwrap();
    }

    #[tokio::test]
    async fn sign_event_over_uds_load() {
        let (nip70, keys) = TestNip70Implementation::new_with_generated_keys();
        let (server, client, server_keypair) =
            get_nip70_server_and_client_test_pair(Arc::from(nip70));

        let pubkey = keys.public_key();

        let mut client_handles = Vec::new();
        for i in 0..128 {
            let client = client.clone();
            let server_keypair = server_keypair.clone();
            let handle = tokio::spawn(async move {
                for j in 0..20 {
                    let created_at = Timestamp::now();
                    let kind = Kind::TextNote;
                    let tags = vec![];
                    let content = format!("Message {} from thread {}.", j, i);
                    let unsigned_event = UnsignedEvent {
                        id: EventId::new(&pubkey, created_at, &kind, &tags, &content),
                        pubkey,
                        created_at,
                        kind,
                        tags,
                        content,
                    };

                    let event = client
                        .sign_event(unsigned_event.clone(), server_keypair.public_key())
                        .await
                        .unwrap();

                    assert!(event.verify().is_ok());
                    assert_eq!(event.id, unsigned_event.id);

                    // Give other client tasks a chance to send requests.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            });
            client_handles.push(handle);
        }

        for handle in client_handles {
            handle.await.unwrap();
        }

        server.stop();
    }

    #[tokio::test]
    async fn make_rpc_with_no_server() {
        let client = Nip70Client::new_internal(get_free_uds_address());
        let keys = Keys::generate();

        let pubkey = keys.public_key();
        let created_at = Timestamp::now();
        let kind = Kind::TextNote;
        let tags = vec![];
        let content = String::from("Hello, world!");
        let unsigned_event = UnsignedEvent {
            id: EventId::new(&pubkey, created_at, &kind, &tags, &content),
            pubkey,
            created_at,
            kind,
            tags,
            content,
        };

        assert_eq!(
            client.sign_event(unsigned_event, pubkey).await,
            Err(Nip70ClientError::UdsClientError(
                UdsClientError::ServerNotRunning
            ))
        );
    }

    #[tokio::test]
    async fn make_rpc_with_rejected_request() {
        let (nip70, keys) = TestNip70Implementation::new_rejecting_all_requests();
        let (server, client, server_keypair) =
            get_nip70_server_and_client_test_pair(Arc::from(nip70));

        let pubkey = keys.public_key();
        let created_at = Timestamp::now();
        let kind = Kind::TextNote;
        let tags = vec![];
        let content = String::from("Hello, world!");
        let unsigned_event = UnsignedEvent {
            id: EventId::new(&pubkey, created_at, &kind, &tags, &content),
            pubkey,
            created_at,
            kind,
            tags,
            content,
        };

        assert_eq!(
            client
                .sign_event(unsigned_event, server_keypair.public_key())
                .await,
            Err(Nip70ClientError::ServerError(Nip70ServerError::Rejected))
        );

        server.stop();
    }
}

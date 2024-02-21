use async_trait::async_trait;
use json_rpc::uds::client::{UdsClientError, UnixDomainSocketJsonRpcClientTransport};
use json_rpc::uds::server::UnixDomainSocketJsonRpcServerTransport;
use json_rpc::{
    JsonRpcClientTransport, JsonRpcError, JsonRpcErrorCode, JsonRpcId, JsonRpcRequest,
    JsonRpcResponse, JsonRpcResponseData, JsonRpcServer, JsonRpcServerHandler,
    JsonRpcStructuredValue,
};
use lightning_invoice::Bolt11Invoice;
use nostr_sdk::secp256k1::XOnlyPublicKey;
use nostr_sdk::{Event, UnsignedEvent};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::json;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

mod json_rpc;

const NIP70_UDS_ADDRESS: &str = "/tmp/nip-70.sock";

const METHOD_NAME_GET_PUBLIC_KEY: &str = "getPublicKey";
const METHOD_NAME_SIGN_EVENT: &str = "signEvent";
const METHOD_NAME_PAY_INVOICE: &str = "payInvoice";
const METHOD_NAME_GET_RELAYS: &str = "getRelays";

/// Errors that can be returned from [`Nip70`] trait functions.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub enum Nip70ServerError {
    /// The server rejected the request. This most likely means that the user
    /// declined to perform the operation for the app that requested it.
    Rejected,

    /// The server encountered an internal error while processing the request.
    InternalError,
}

impl Nip70ServerError {
    fn to_json_rpc_error(&self) -> JsonRpcError {
        match self {
            Nip70ServerError::Rejected => JsonRpcError {
                code: JsonRpcErrorCode::Custom(1),
                message: "Rejected".to_string(),
                data: None,
            },
            Nip70ServerError::InternalError => JsonRpcError {
                code: JsonRpcErrorCode::Custom(2),
                message: "Internal error".to_string(),
                data: None,
            },
        }
    }

    fn from_json_rpc_error(error: &JsonRpcError) -> Result<Self, JsonRpcError> {
        match error.code {
            JsonRpcErrorCode::Custom(1) => Ok(Nip70ServerError::Rejected),
            JsonRpcErrorCode::Custom(2) => Ok(Nip70ServerError::InternalError),
            _ => Err(error.clone()),
        }
    }
}

// Defines the server-side functionality for the NIP-70 protocol.
// Implement this trait and pass it to `Nip70Server::new()` to run a NIP-70 server.
#[async_trait]
pub trait Nip70: Send + Sync {
    // -----------------
    // Required methods.
    // -----------------

    /// Returns the public key of the signed-in user.
    async fn get_public_key(&self) -> Result<XOnlyPublicKey, Nip70ServerError>;

    /// Signs a Nostr event on behalf of the signed-in user.
    async fn sign_event(&self, event: UnsignedEvent) -> Result<Event, Nip70ServerError>;

    /// Pays an invoice.
    async fn pay_invoice(
        &self,
        pay_invoice_request: PayInvoiceRequest,
    ) -> Result<PayInvoiceResponse, Nip70ServerError>;

    // -----------------
    // Optional methods.
    // -----------------

    // Returns the list of relays that the server is aware of, or `None` if
    // the server does not support this feature.
    async fn get_relays(&self) -> Result<Option<HashMap<String, RelayPolicy>>, Nip70ServerError> {
        Ok(None)
    }
}

// Runs a NIP-70 compliant Unix domain socket server.
pub fn run_nip70_server(nip70: Arc<dyn Nip70>) -> std::io::Result<JsonRpcServer> {
    run_nip70_server_internal(nip70, NIP70_UDS_ADDRESS.to_string())
}

fn run_nip70_server_internal(
    nip70: Arc<dyn Nip70>,
    uds_address: String,
) -> std::io::Result<JsonRpcServer> {
    Ok(JsonRpcServer::new(
        Box::from(UnixDomainSocketJsonRpcServerTransport::connect_and_start(
            uds_address,
        )?),
        Box::from(Nip70ServerHandler { nip70 }),
    ))
}

struct Nip70ServerHandler {
    nip70: Arc<dyn Nip70>,
}

#[async_trait::async_trait]
impl JsonRpcServerHandler for Nip70ServerHandler {
    async fn handle_batch_request(&self, requests: Vec<JsonRpcRequest>) -> Vec<JsonRpcResponse> {
        let mut responses = Vec::new();

        for request in requests {
            let parsed_request = match Nip70Request::from_json_rpc_request(&request) {
                Ok(request) => request,
                Err(error) => {
                    responses.push(JsonRpcResponse::new(
                        JsonRpcResponseData::Error { error },
                        request.id().clone(),
                    ));
                    continue;
                }
            };

            let response = match parsed_request {
                Nip70Request::GetPublicKey => match self.nip70.get_public_key().await {
                    Ok(public_key) => Nip70Response::PublicKey(public_key),
                    Err(err) => Nip70Response::Error(err),
                },
                // TODO: Let's get the pubkey and check it against the unsigned event before signing.
                Nip70Request::SignEvent(event) => match self.nip70.sign_event(event).await {
                    Ok(event) => Nip70Response::Event(event),
                    Err(err) => Nip70Response::Error(err),
                },
                Nip70Request::PayInvoice(pay_invoice_request) => {
                    match self.nip70.pay_invoice(pay_invoice_request).await {
                        Ok(pay_invoice_response) => {
                            Nip70Response::InvoicePaid(pay_invoice_response)
                        }
                        Err(err) => Nip70Response::Error(err),
                    }
                }
                Nip70Request::GetRelays => match self.nip70.get_relays().await {
                    Ok(relays) => Nip70Response::Relays(relays),
                    Err(err) => Nip70Response::Error(err),
                },
            };

            responses.push(response.to_json_rpc_response(request.id().clone()));
        }

        responses
    }
}

#[derive(PartialEq, Debug)]
pub enum Nip70ClientError {
    UdsClientError(UdsClientError),
    ProtocolError,
    ServerError(Nip70ServerError),
}

/// Fetches the public key of the signed-in user from the NIP-70 server.
/// If no server is running, returns `Err(Nip70ClientError::UdsClientError(UdsClientError::ServerNotRunning))`.
pub async fn get_public_key() -> Result<XOnlyPublicKey, Nip70ClientError> {
    get_public_key_internal(NIP70_UDS_ADDRESS.to_string()).await
}

async fn get_public_key_internal(uds_address: String) -> Result<XOnlyPublicKey, Nip70ClientError> {
    // TODO: Get a real request id.
    let json_rpc_request = Nip70Request::GetPublicKey.to_json_rpc_request(JsonRpcId::Number(1234));
    let json_rpc_response = UnixDomainSocketJsonRpcClientTransport::new(uds_address)
        .send_request(json_rpc_request.clone())
        .await
        .map_err(Nip70ClientError::UdsClientError)?;
    match Nip70Response::from_json_rpc_response(&json_rpc_request, &json_rpc_response)
        .map_err(|_| Nip70ClientError::ProtocolError)?
    {
        Nip70Response::PublicKey(response) => Ok(response),
        Nip70Response::Error(err) => Err(Nip70ClientError::ServerError(err)),
        _ => Err(Nip70ClientError::ProtocolError),
    }
}

/// Signs a Nostr event on behalf of the signed-in user using the NIP-70 server.
/// If no server is running, returns `Err(Nip70ClientError::ServerNotRunning)`.
pub async fn sign_event(event: UnsignedEvent) -> Result<Event, Nip70ClientError> {
    sign_event_internal(event, NIP70_UDS_ADDRESS.to_string()).await
}

async fn sign_event_internal(
    event: UnsignedEvent,
    uds_address: String,
) -> Result<Event, Nip70ClientError> {
    // TODO: Get a real request id.
    let json_rpc_request =
        Nip70Request::SignEvent(event).to_json_rpc_request(JsonRpcId::Number(1234));
    let json_rpc_response = UnixDomainSocketJsonRpcClientTransport::new(uds_address)
        .send_request(json_rpc_request.clone())
        .await
        .map_err(Nip70ClientError::UdsClientError)?;
    match Nip70Response::from_json_rpc_response(&json_rpc_request, &json_rpc_response)
        .map_err(|_| Nip70ClientError::ProtocolError)?
    {
        Nip70Response::Event(response) => Ok(response),
        Nip70Response::Error(err) => Err(Nip70ClientError::ServerError(err)),
        _ => Err(Nip70ClientError::ProtocolError),
    }
}

/// Pays an invoice using the NIP-70 server.
pub async fn pay_invoice(
    request: PayInvoiceRequest,
) -> Result<PayInvoiceResponse, Nip70ClientError> {
    pay_invoice_internal(request, NIP70_UDS_ADDRESS.to_string()).await
}

async fn pay_invoice_internal(
    request: PayInvoiceRequest,
    uds_address: String,
) -> Result<PayInvoiceResponse, Nip70ClientError> {
    // TODO: Get a real request id.
    let json_rpc_request =
        Nip70Request::PayInvoice(request).to_json_rpc_request(JsonRpcId::Number(1234));
    let json_rpc_response = UnixDomainSocketJsonRpcClientTransport::new(uds_address)
        .send_request(json_rpc_request.clone())
        .await
        .map_err(Nip70ClientError::UdsClientError)?;
    match Nip70Response::from_json_rpc_response(&json_rpc_request, &json_rpc_response)
        .map_err(|_| Nip70ClientError::ProtocolError)?
    {
        Nip70Response::InvoicePaid(response) => Ok(response),
        Nip70Response::Error(err) => Err(Nip70ClientError::ServerError(err)),
        _ => Err(Nip70ClientError::ProtocolError),
    }
}

/// Fetches the list of relays that the NIP-70 server is aware of.
/// If no server is running, returns `Err(Nip70ClientError::ServerNotRunning)`.
/// If the server does not support this feature, returns `Ok(None)`.
pub async fn get_relays() -> Result<Option<HashMap<String, RelayPolicy>>, Nip70ClientError> {
    get_relays_internal(NIP70_UDS_ADDRESS.to_string()).await
}

async fn get_relays_internal(
    uds_address: String,
) -> Result<Option<HashMap<String, RelayPolicy>>, Nip70ClientError> {
    // TODO: Get a real request id.
    let json_rpc_request = Nip70Request::GetRelays.to_json_rpc_request(JsonRpcId::Number(1234));
    let json_rpc_response = UnixDomainSocketJsonRpcClientTransport::new(uds_address)
        .send_request(json_rpc_request.clone())
        .await
        .map_err(Nip70ClientError::UdsClientError)?;
    match Nip70Response::from_json_rpc_response(&json_rpc_request, &json_rpc_response)
        .map_err(|_| Nip70ClientError::ProtocolError)?
    {
        Nip70Response::Relays(response) => Ok(response),
        Nip70Response::Error(err) => Err(Nip70ClientError::ServerError(err)),
        _ => Err(Nip70ClientError::ProtocolError),
    }
}

#[derive(Serialize, Deserialize)]
enum Nip70Request {
    GetPublicKey,
    SignEvent(UnsignedEvent),
    PayInvoice(PayInvoiceRequest),
    GetRelays,
}

impl Nip70Request {
    fn to_json_rpc_request(&self, request_id: JsonRpcId) -> JsonRpcRequest {
        match self {
            Nip70Request::GetPublicKey => {
                JsonRpcRequest::new(METHOD_NAME_GET_PUBLIC_KEY.to_string(), None, request_id)
            }
            Nip70Request::SignEvent(event) => JsonRpcRequest::new(
                METHOD_NAME_SIGN_EVENT.to_string(),
                Some(JsonRpcStructuredValue::Object(
                    // This should never panic, since we're converting an `UnsignedEvent`
                    // struct, which should always serialize to a JSON object.
                    json!(event)
                        .as_object()
                        .expect("Failed to convert event to object")
                        .clone(),
                )),
                request_id,
            ),
            Nip70Request::PayInvoice(request) => JsonRpcRequest::new(
                METHOD_NAME_PAY_INVOICE.to_string(),
                Some(JsonRpcStructuredValue::Object(
                    // This should never panic, since we're converting a `PayInvoiceRequest`
                    // struct, which should always serialize to a JSON object.
                    json!(request)
                        .as_object()
                        .expect("Failed to convert request to object")
                        .clone(),
                )),
                request_id,
            ),
            Nip70Request::GetRelays => {
                JsonRpcRequest::new(METHOD_NAME_GET_RELAYS.to_string(), None, request_id)
            }
        }
    }

    fn from_json_rpc_request(request: &JsonRpcRequest) -> Result<Self, JsonRpcError> {
        match request.method() {
            METHOD_NAME_GET_PUBLIC_KEY => Ok(Nip70Request::GetPublicKey),
            METHOD_NAME_SIGN_EVENT => Ok(Nip70Request::SignEvent(
                if let Ok(value) =
                    serde_json::from_value(match request.params().map(|v| v.clone().into_value()) {
                        Some(value) => value,
                        None => {
                            return Err(JsonRpcError {
                                code: JsonRpcErrorCode::InternalError,
                                message: "Internal error".to_string(),
                                data: None,
                            })
                        }
                    })
                {
                    value
                } else {
                    return Err(JsonRpcError {
                        code: JsonRpcErrorCode::InternalError,
                        message: "Internal error".to_string(),
                        data: None,
                    });
                },
            )),
            METHOD_NAME_PAY_INVOICE => Ok(Nip70Request::PayInvoice(
                if let Ok(value) =
                    serde_json::from_value(match request.params().map(|v| v.clone().into_value()) {
                        Some(value) => value,
                        None => {
                            return Err(JsonRpcError {
                                code: JsonRpcErrorCode::InternalError,
                                message: "Internal error".to_string(),
                                data: None,
                            })
                        }
                    })
                {
                    value
                } else {
                    return Err(JsonRpcError {
                        code: JsonRpcErrorCode::InternalError,
                        message: "Internal error".to_string(),
                        data: None,
                    });
                },
            )),
            METHOD_NAME_GET_RELAYS => Ok(Nip70Request::GetRelays),
            _ => Err(JsonRpcError {
                code: JsonRpcErrorCode::MethodNotFound,
                message: "Method not found".to_string(),
                data: None,
            }),
        }
    }
}

#[derive(Serialize, Deserialize)]
enum Nip70Response {
    PublicKey(XOnlyPublicKey),
    Event(Event),
    InvoicePaid(PayInvoiceResponse),
    Relays(Option<HashMap<String, RelayPolicy>>),
    Error(Nip70ServerError),
}

impl Nip70Response {
    fn to_json_rpc_response(&self, request_id: JsonRpcId) -> JsonRpcResponse {
        match self {
            Nip70Response::PublicKey(response) => JsonRpcResponse::new(
                JsonRpcResponseData::Success {
                    result: serde_json::to_value(response).unwrap(),
                },
                request_id,
            ),
            Nip70Response::Event(response) => JsonRpcResponse::new(
                JsonRpcResponseData::Success {
                    result: serde_json::to_value(response).unwrap(),
                },
                request_id,
            ),
            Nip70Response::InvoicePaid(response) => JsonRpcResponse::new(
                JsonRpcResponseData::Success {
                    result: serde_json::to_value(response).unwrap(),
                },
                request_id,
            ),
            Nip70Response::Relays(response) => JsonRpcResponse::new(
                JsonRpcResponseData::Success {
                    result: serde_json::to_value(response).unwrap(),
                },
                request_id,
            ),
            Nip70Response::Error(err) => JsonRpcResponse::new(
                JsonRpcResponseData::Error {
                    error: err.to_json_rpc_error(),
                },
                request_id,
            ),
        }
    }

    fn from_json_rpc_response(
        request: &JsonRpcRequest,
        response: &JsonRpcResponse,
    ) -> Result<Self, JsonRpcError> {
        let result = match response.data() {
            JsonRpcResponseData::Success { result } => result,
            JsonRpcResponseData::Error { error } => {
                return match Nip70ServerError::from_json_rpc_error(error) {
                    Ok(err) => Ok(Nip70Response::Error(err)),
                    Err(err) => Err(err),
                }
            }
        };

        match request.method() {
            METHOD_NAME_GET_PUBLIC_KEY => Ok(Nip70Response::PublicKey(
                if let Ok(value) = serde_json::from_value(result.clone()) {
                    value
                } else {
                    return Err(JsonRpcError {
                        code: JsonRpcErrorCode::InternalError,
                        message: "Internal error".to_string(),
                        data: None,
                    });
                },
            )),
            METHOD_NAME_SIGN_EVENT => Ok(Nip70Response::Event(
                if let Ok(value) = serde_json::from_value(result.clone()) {
                    value
                } else {
                    return Err(JsonRpcError {
                        code: JsonRpcErrorCode::InternalError,
                        message: "Internal error".to_string(),
                        data: None,
                    });
                },
            )),
            METHOD_NAME_PAY_INVOICE => Ok(Nip70Response::InvoicePaid(
                if let Ok(value) = serde_json::from_value(result.clone()) {
                    value
                } else {
                    return Err(JsonRpcError {
                        code: JsonRpcErrorCode::InternalError,
                        message: "Internal error".to_string(),
                        data: None,
                    });
                },
            )),
            METHOD_NAME_GET_RELAYS => Ok(Nip70Response::Relays(
                if let Ok(value) = serde_json::from_value(result.clone()) {
                    value
                } else {
                    return Err(JsonRpcError {
                        code: JsonRpcErrorCode::InternalError,
                        message: "Internal error".to_string(),
                        data: None,
                    });
                },
            )),
            _ => Err(JsonRpcError {
                code: JsonRpcErrorCode::MethodNotFound,
                message: "Method not found".to_string(),
                data: None,
            }),
        }
    }
}

/// A policy that specifies whether a relay is allowed to read or write to the server.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct RelayPolicy {
    read: bool,
    write: bool,
}

/// A request to pay an invoice.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct PayInvoiceRequest {
    /// Bolt11 invoice to pay.
    #[serde(
        serialize_with = "serialize_to_string",
        deserialize_with = "deserialize_from_string"
    )]
    invoice: Bolt11Invoice,
}

impl PayInvoiceRequest {
    pub fn new(invoice: Bolt11Invoice) -> Self {
        Self { invoice }
    }

    pub fn invoice(&self) -> &Bolt11Invoice {
        &self.invoice
    }
}

fn serialize_to_string<S>(value: &Bolt11Invoice, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&value.to_string())
}

fn deserialize_from_string<'de, D>(deserializer: D) -> Result<Bolt11Invoice, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Bolt11Invoice::from_str(&s).map_err(serde::de::Error::custom)
}

/// A response to a pay invoice request.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub enum PayInvoiceResponse {
    /// The invoice was paid successfully. Contains the preimage of the payment.
    Success(String),

    /// The invoice was not paid successfully. Contains the reason for the failure.
    ErrorPaymentFailed(String),

    /// The invoice was malformed and could not be paid.
    ErrorMalformedInvoice,
}

#[cfg(test)]
mod tests {
    use super::*;

    use bitcoin_hashes::Hash;
    use lightning_invoice::{Currency, InvoiceBuilder, PaymentSecret};
    use nostr_sdk::{
        secp256k1::{Secp256k1, SecretKey},
        EventId, Keys, Kind, Timestamp,
    };
    use std::{sync::Mutex, time::Duration};

    struct TestNip70Implementation {
        keys: Keys,
        reject_all_requests: bool,
    }

    impl TestNip70Implementation {
        fn new_with_generated_keys() -> Self {
            Self {
                keys: Keys::generate(),
                reject_all_requests: false,
            }
        }

        fn new_rejecting_all_requests() -> Self {
            Self {
                keys: Keys::generate(),
                reject_all_requests: true,
            }
        }
    }

    #[async_trait]
    impl Nip70 for TestNip70Implementation {
        async fn get_public_key(&self) -> Result<XOnlyPublicKey, Nip70ServerError> {
            if self.reject_all_requests {
                return Err(Nip70ServerError::Rejected);
            }

            Ok(self.keys.public_key())
        }

        async fn sign_event(&self, event: UnsignedEvent) -> Result<Event, Nip70ServerError> {
            if self.reject_all_requests {
                return Err(Nip70ServerError::Rejected);
            }

            event
                .sign(&self.keys)
                .map_err(|_| Nip70ServerError::InternalError)
        }

        async fn pay_invoice(
            &self,
            pay_invoice_request: PayInvoiceRequest,
        ) -> Result<PayInvoiceResponse, Nip70ServerError> {
            if self.reject_all_requests {
                return Err(Nip70ServerError::Rejected);
            }

            Ok(PayInvoiceResponse::Success(format!(
                "preimage for invoice {}",
                pay_invoice_request.invoice
            )))
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

    #[tokio::test]
    async fn get_public_key_over_uds() {
        let uds_address = get_free_uds_address();
        let nip70 = Arc::from(TestNip70Implementation::new_with_generated_keys());
        let server = run_nip70_server_internal(nip70.clone(), uds_address.clone()).unwrap();

        assert_eq!(
            nip70.get_public_key().await.unwrap(),
            get_public_key_internal(uds_address).await.unwrap()
        );

        server.stop();
    }

    #[tokio::test]
    async fn sign_event_over_uds() {
        let uds_address = get_free_uds_address();
        let nip70 = Arc::from(TestNip70Implementation::new_with_generated_keys());
        let server = run_nip70_server_internal(nip70.clone(), uds_address.clone()).unwrap();

        let pubkey = get_public_key_internal(uds_address.clone()).await.unwrap();
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

        let event = sign_event_internal(unsigned_event, uds_address)
            .await
            .unwrap();

        assert!(event.verify().is_ok());

        server.stop();
    }

    fn get_test_invoice() -> Bolt11Invoice {
        let private_key = SecretKey::from_slice(
            &[
                0xe1, 0x26, 0xf6, 0x8f, 0x7e, 0xaf, 0xcc, 0x8b, 0x74, 0xf5, 0x4d, 0x26, 0x9f, 0xe2,
                0x06, 0xbe, 0x71, 0x50, 0x00, 0xf9, 0x4d, 0xac, 0x06, 0x7d, 0x1c, 0x04, 0xa8, 0xca,
                0x3b, 0x2d, 0xb7, 0x34,
            ][..],
        )
        .unwrap();

        let payment_hash = bitcoin_hashes::sha256::Hash::from_slice(&[0; 32][..]).unwrap();
        let payment_secret = PaymentSecret([42u8; 32]);

        InvoiceBuilder::new(Currency::Bitcoin)
            .description("Coins pls!".into())
            .payment_hash(payment_hash)
            .payment_secret(payment_secret)
            .current_timestamp()
            .min_final_cltv_expiry_delta(144)
            .build_signed(|hash| Secp256k1::new().sign_ecdsa_recoverable(hash, &private_key))
            .unwrap()
    }

    #[tokio::test]
    async fn pay_invoice_over_uds() {
        let uds_address = get_free_uds_address();
        let nip70 = Arc::from(TestNip70Implementation::new_with_generated_keys());
        let server = run_nip70_server_internal(nip70.clone(), uds_address.clone()).unwrap();

        let invoice = get_test_invoice();

        let pay_invoice_response = pay_invoice_internal(
            PayInvoiceRequest {
                invoice: invoice.clone(),
            },
            uds_address,
        )
        .await
        .unwrap();

        assert_eq!(
            pay_invoice_response,
            PayInvoiceResponse::Success(format!("preimage for invoice {invoice}"))
        );

        server.stop();
    }

    #[tokio::test]
    async fn sign_large_event_over_uds() {
        let uds_address = get_free_uds_address();
        let nip70 = Arc::from(TestNip70Implementation::new_with_generated_keys());
        let server = run_nip70_server_internal(nip70.clone(), uds_address.clone()).unwrap();

        let pubkey = get_public_key_internal(uds_address.clone()).await.unwrap();
        let created_at = Timestamp::now();
        let kind = Kind::TextNote;
        let tags = vec![];
        let content: String = std::iter::repeat('a').take((2 as usize).pow(25)).collect();
        let unsigned_event = UnsignedEvent {
            id: EventId::new(&pubkey, created_at, &kind, &tags, &content),
            pubkey,
            created_at,
            kind,
            tags,
            content,
        };

        let event = sign_event_internal(unsigned_event, uds_address)
            .await
            .unwrap();

        assert!(event.verify().is_ok());

        server.stop();
    }

    #[test]
    #[should_panic(expected = "must be called from the context of a Tokio 1.x runtime")]
    fn run_server_without_async_runtime() {
        let uds_address = get_free_uds_address();
        let nip70 = Arc::from(TestNip70Implementation::new_with_generated_keys());
        run_nip70_server_internal(nip70.clone(), uds_address.clone()).unwrap();
    }

    #[tokio::test]
    async fn sign_event_over_uds_load() {
        let uds_address = get_free_uds_address();
        let nip70 = Arc::from(TestNip70Implementation::new_with_generated_keys());
        let server = run_nip70_server_internal(nip70.clone(), uds_address.clone()).unwrap();

        let mut client_handles = Vec::new();
        for i in 0..128 {
            let uds_address = uds_address.clone();
            let handle = tokio::spawn(async move {
                for j in 0..20 {
                    let uds_address = uds_address.clone();
                    let pubkey = get_public_key_internal(uds_address.clone()).await.unwrap();
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

                    let event = sign_event_internal(unsigned_event.clone(), uds_address)
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
        let public_key_or = get_public_key().await;
        assert!(public_key_or.is_err());
        assert_eq!(
            public_key_or.unwrap_err(),
            Nip70ClientError::UdsClientError(UdsClientError::ServerNotRunning)
        );
    }

    #[tokio::test]
    async fn make_rpc_with_rejected_request() {
        let uds_address = get_free_uds_address();
        let nip70 = Arc::from(TestNip70Implementation::new_rejecting_all_requests());
        let server = run_nip70_server_internal(nip70, uds_address.clone()).unwrap();

        let public_key_or = get_public_key_internal(uds_address).await;
        assert!(public_key_or.is_err());
        assert_eq!(
            public_key_or.unwrap_err(),
            Nip70ClientError::ServerError(Nip70ServerError::Rejected)
        );

        server.stop();
    }
}

use async_trait::async_trait;
use nostr_sdk::secp256k1::XOnlyPublicKey;
use nostr_sdk::{Event, UnsignedEvent};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

const NIP70_UDS_ADDRESS: &str = "/tmp/nip70.sock";
const BUFFER_SIZE: usize = 1024;

// Defines the server-side functionality for the NIP-70 protocol.
#[async_trait]
pub trait Nip70: Send + Sync {
    // -----------------
    // Required methods.
    // -----------------

    /// Returns the public key of the signed-in user.
    async fn get_public_key(&self) -> anyhow::Result<XOnlyPublicKey>;

    /// Signs a Nostr event on behalf of the signed-in user.
    async fn sign_event(&self, event: UnsignedEvent) -> anyhow::Result<Event>;

    // -----------------
    // Optional methods.
    // -----------------

    // Returns the list of relays that the server is aware of, or `None` if
    // the server does not support this feature.
    async fn get_relays(&self) -> anyhow::Result<Option<HashMap<String, RelayPolicy>>> {
        Ok(None)
    }
}

// Accepts a `Nip70` implementation and binds to a Unix domain socket.
pub struct Nip70Server {
    uds_task_handle: tokio::task::JoinHandle<()>,
    uds_address: String,
}

impl Nip70Server {
    /// Creates a new `Nip70Server` instance and binds to a Unix domain socket.
    pub fn new(nip70: Arc<dyn Nip70>) -> anyhow::Result<Self> {
        Self::new_internal(nip70, NIP70_UDS_ADDRESS.to_string())
    }

    fn new_internal(nip70: Arc<dyn Nip70>, uds_address: String) -> anyhow::Result<Self> {
        if Path::new(&uds_address).exists() {
            std::fs::remove_file(&uds_address)?;
        }

        let listener = UnixListener::bind(&uds_address)?;

        let uds_task_handle = tokio::spawn(async move {
            loop {
                let inner_nip70 = nip70.clone();
                if let Ok((mut socket, _)) = listener.accept().await {
                    // TODO: Grab the task handle and cancel it when the server is dropped.
                    let handle: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
                        let mut buf = vec![0; BUFFER_SIZE];
                        let mut rolling_buf: Vec<u8> = Vec::new();
                        let request: Nip70Request;

                        // TODO: Add a timeout to this read operation.
                        loop {
                            socket.readable().await?;
                            if let Ok(nbytes) = socket.try_read(&mut buf) {
                                for byte in &buf[..nbytes] {
                                    rolling_buf.push(*byte);
                                }
                                if let Ok(parsed_request) =
                                    serde_json::from_slice::<Nip70Request>(&rolling_buf)
                                {
                                    request = parsed_request;
                                    break;
                                }
                            };
                        }

                        let response_or = Self::handle_incoming_request(request, inner_nip70).await;

                        socket.writable().await?;
                        // TODO: Handle the error case, and write an error response to the socket.
                        if let Ok(response) = response_or {
                            if let Ok(serialized_response) = serde_json::to_vec(&response) {
                                socket.write_all(&serialized_response).await?;
                                socket.shutdown().await?;
                            }
                        }

                        Ok(())
                    });
                }
            }
        });

        Ok(Nip70Server {
            uds_task_handle,
            uds_address,
        })
    }

    async fn handle_incoming_request(
        request: Nip70Request,
        nip70: Arc<dyn Nip70>,
    ) -> anyhow::Result<Nip70Response> {
        Ok(match request {
            Nip70Request::GetPublicKey => Nip70Response::PublicKey(nip70.get_public_key().await?),
            // TODO: Let's get the pubkey and check it against the unsigned event before signing.
            Nip70Request::SignEvent(event) => Nip70Response::Event(nip70.sign_event(event).await?),
            Nip70Request::GetRelays => Nip70Response::Relays(nip70.get_relays().await?),
        })
    }
}

impl std::ops::Drop for Nip70Server {
    fn drop(&mut self) {
        // Abort the UDS task, since it will loop forever otherwise.
        self.uds_task_handle.abort();

        // Try to remove the UDS file. If it fails, it's not a big deal.
        let _ = std::fs::remove_file(&self.uds_address);
    }
}

// TODO: Test error handling more thoroughly.
#[derive(Debug, PartialEq)]
pub enum Nip70Error {
    /// The NIP-70 Unix domain socket server is not running.
    ServerNotRunning,

    /// An I/O error occurred while writing to or reading from the Unix domain socket.
    UdsSocketError,

    /// A NIP-70 protocol-level error occurred while encoding or decoding messages.
    ProtocolError,
}

impl std::fmt::Display for Nip70Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Nip70Error::ServerNotRunning => {
                write!(f, "NIP-70 Unix domain socket server not running.")
            }
            Nip70Error::UdsSocketError => {
                write!(f, "Error writing to or reading from Unix domain socket.")
            }
            Nip70Error::ProtocolError => {
                write!(
                    f,
                    "Error encoding or decoding messages according to the NIP-70 protocol."
                )
            }
        }
    }
}

impl std::error::Error for Nip70Error {}

async fn make_rpc(request: &Nip70Request, uds_address: &str) -> Result<Nip70Response, Nip70Error> {
    // Open up a UDS connection to the server.
    let mut socket = UnixStream::connect(uds_address)
        .await
        .map_err(|_| Nip70Error::ServerNotRunning)?;

    // Send the request.
    let serialized_request = &serde_json::to_vec(request).map_err(|_| Nip70Error::ProtocolError)?;
    let mut bytes_written = 0;
    while bytes_written < serialized_request.len() {
        socket
            .writable()
            .await
            .map_err(|_| Nip70Error::UdsSocketError)?;
        bytes_written += socket
            .try_write(&serialized_request[bytes_written..])
            .unwrap_or(0);
    }
    socket
        .flush()
        .await
        .map_err(|_| Nip70Error::UdsSocketError)?;

    // Read the response from the server.
    // TODO: Add a timeout to this read operation.
    socket
        .readable()
        .await
        .map_err(|_| Nip70Error::UdsSocketError)?;
    let mut buf = Vec::new();
    socket
        .read_to_end(&mut buf)
        .await
        .map_err(|_| Nip70Error::UdsSocketError)?;
    Ok(serde_json::from_slice::<Nip70Response>(&buf).map_err(|_| Nip70Error::ProtocolError)?)
}

pub async fn get_public_key() -> Result<XOnlyPublicKey, Nip70Error> {
    get_public_key_internal(NIP70_UDS_ADDRESS).await
}

async fn get_public_key_internal(uds_address: &str) -> Result<XOnlyPublicKey, Nip70Error> {
    let response = make_rpc(&Nip70Request::GetPublicKey, uds_address).await?;
    if let Nip70Response::PublicKey(public_key) = response {
        Ok(public_key)
    } else {
        Err(Nip70Error::ProtocolError)
    }
}

pub async fn sign_event(event: UnsignedEvent) -> Result<Event, Nip70Error> {
    sign_event_internal(event, NIP70_UDS_ADDRESS).await
}

async fn sign_event_internal(event: UnsignedEvent, uds_address: &str) -> Result<Event, Nip70Error> {
    let response = make_rpc(&Nip70Request::SignEvent(event), uds_address).await?;
    if let Nip70Response::Event(event) = response {
        Ok(event)
    } else {
        Err(Nip70Error::ProtocolError)
    }
}

pub async fn get_relays() -> Result<Option<HashMap<String, RelayPolicy>>, Nip70Error> {
    get_relays_internal(NIP70_UDS_ADDRESS).await
}

async fn get_relays_internal(
    uds_address: &str,
) -> Result<Option<HashMap<String, RelayPolicy>>, Nip70Error> {
    let response = make_rpc(&Nip70Request::GetRelays, uds_address).await?;
    if let Nip70Response::Relays(relays) = response {
        Ok(relays)
    } else {
        Err(Nip70Error::ProtocolError)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
enum Nip70Request {
    #[serde(rename = "pubKey")]
    GetPublicKey,
    SignEvent(UnsignedEvent),
    GetRelays,
}

#[derive(serde::Serialize, serde::Deserialize)]
enum Nip70Response {
    #[serde(rename = "pubKey")]
    PublicKey(XOnlyPublicKey),
    Event(Event),
    Relays(Option<HashMap<String, RelayPolicy>>),
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct RelayPolicy {
    read: bool,
    write: bool,
}

#[cfg(test)]
mod tests {
    use std::{sync::Mutex, time::Duration};

    use super::*;

    use nostr_sdk::{EventId, Keys, Kind, Timestamp};

    struct TestNip70Implementation {
        keys: Keys,
    }

    impl TestNip70Implementation {
        fn new_with_generated_keys() -> Self {
            Self {
                keys: Keys::generate(),
            }
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

    #[async_trait]
    impl Nip70 for TestNip70Implementation {
        async fn get_public_key(&self) -> anyhow::Result<XOnlyPublicKey> {
            Ok(self.keys.public_key())
        }

        async fn sign_event(&self, event: UnsignedEvent) -> anyhow::Result<Event> {
            Ok(event.sign(&self.keys)?)
        }
    }

    #[tokio::test]
    async fn get_public_key_over_uds() {
        let uds_address = get_free_uds_address();
        let nip70 = Arc::from(TestNip70Implementation::new_with_generated_keys());
        let server = Nip70Server::new_internal(nip70.clone(), uds_address.clone()).unwrap();

        assert_eq!(
            nip70.get_public_key().await.unwrap(),
            get_public_key_internal(&uds_address).await.unwrap()
        );
    }

    #[tokio::test]
    async fn sign_event_over_uds() {
        let uds_address = get_free_uds_address();
        let nip70 = Arc::from(TestNip70Implementation::new_with_generated_keys());
        let server = Nip70Server::new_internal(nip70.clone(), uds_address.clone()).unwrap();

        let pubkey = get_public_key_internal(&uds_address).await.unwrap();
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

        let event = sign_event_internal(unsigned_event, &uds_address)
            .await
            .unwrap();

        assert!(event.verify().is_ok());
    }

    #[tokio::test]
    async fn sign_large_event_over_uds() {
        let uds_address = get_free_uds_address();
        let nip70 = Arc::from(TestNip70Implementation::new_with_generated_keys());
        let server = Nip70Server::new_internal(nip70.clone(), uds_address.clone()).unwrap();

        let pubkey = get_public_key_internal(&uds_address).await.unwrap();
        let created_at = Timestamp::now();
        let kind = Kind::TextNote;
        let tags = vec![];
        let content: String = std::iter::repeat('a').take(BUFFER_SIZE * 1000).collect();
        let unsigned_event = UnsignedEvent {
            id: EventId::new(&pubkey, created_at, &kind, &tags, &content),
            pubkey,
            created_at,
            kind,
            tags,
            content,
        };

        let event = sign_event_internal(unsigned_event, &uds_address)
            .await
            .unwrap();

        assert!(event.verify().is_ok());
    }

    #[tokio::test]
    async fn sign_event_over_uds_load() {
        let uds_address = get_free_uds_address();
        let nip70 = Arc::from(TestNip70Implementation::new_with_generated_keys());
        let server = Nip70Server::new_internal(nip70.clone(), uds_address.clone()).unwrap();

        let mut client_handles = Vec::new();
        for i in 0..128 {
            let uds_address = uds_address.clone();
            let handle = tokio::spawn(async move {
                for j in 0..20 {
                    let pubkey = get_public_key_internal(&uds_address).await.unwrap();
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

                    let event = sign_event_internal(unsigned_event.clone(), &uds_address)
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
    }

    #[tokio::test]
    async fn make_rpc_with_no_server() {
        let public_key_or = get_public_key().await;
        assert!(public_key_or.is_err());
        assert_eq!(public_key_or.unwrap_err(), Nip70Error::ServerNotRunning);
    }
}

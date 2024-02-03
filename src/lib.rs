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
// TODO: Increase this buffer size once we've added unit tests
// that ensure large messages are handled correctly. This should
// significantly increase performance.
const BUFFER_SIZE: usize = 8;

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
}

impl Nip70Server {
    /// Creates a new `Nip70Server` instance and binds to a Unix domain socket.
    /// DO NOT pass in a `Nip70Client` instance here, as it will cause the server
    /// to handle all incoming requests by making a new request to itself, which
    /// will result in infinite looping.
    pub fn new(nip70: Arc<dyn Nip70>) -> anyhow::Result<Self> {
        if Path::new(NIP70_UDS_ADDRESS).exists() {
            std::fs::remove_file(NIP70_UDS_ADDRESS)?;
        }

        let listener = UnixListener::bind(NIP70_UDS_ADDRESS)?;

        let uds_task_handle = tokio::spawn(async move {
            loop {
                let inner_nip70 = nip70.clone();
                if let Ok((mut socket, _)) = listener.accept().await {
                    // TODO: Grab the task handle and cancel it when the server is dropped.
                    let handle: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
                        let mut buf = vec![0; BUFFER_SIZE];
                        let mut rolling_buf: Vec<u8> = Vec::new();
                        let request: Nip70Request;

                        socket.readable().await?;
                        // TODO: Add a timeout to this read operation.
                        loop {
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

        Ok(Nip70Server { uds_task_handle })
    }

    async fn handle_incoming_request(
        request: Nip70Request,
        nip70: Arc<dyn Nip70>,
    ) -> anyhow::Result<Nip70Response> {
        Ok(match request {
            Nip70Request::GetPublicKey => Nip70Response::PublicKey(nip70.get_public_key().await?),
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
        let _ = std::fs::remove_file(NIP70_UDS_ADDRESS);
    }
}

#[derive(Default)]
pub struct Nip70Client {}

impl Nip70Client {
    async fn make_rpc(request: &Nip70Request) -> anyhow::Result<Nip70Response> {
        // Open up a UDS connection to the server.
        let mut socket = UnixStream::connect(NIP70_UDS_ADDRESS).await?;

        // Send the request.
        socket.writable().await?;
        socket.try_write(&serde_json::to_vec(request)?)?;
        socket.flush().await?;

        // Read the response from the server.
        // TODO: Add a timeout to this read operation.
        socket.readable().await?;
        let mut buf = Vec::new();
        socket.read_to_end(&mut buf).await?;
        Ok(serde_json::from_slice::<Nip70Response>(&buf)?)
    }
}

#[async_trait]
impl Nip70 for Nip70Client {
    async fn get_public_key(&self) -> anyhow::Result<XOnlyPublicKey> {
        let response = Self::make_rpc(&Nip70Request::GetPublicKey).await?;
        if let Nip70Response::PublicKey(public_key) = response {
            Ok(public_key)
        } else {
            anyhow::bail!("Unexpected response from server!")
        }
    }

    async fn sign_event(&self, event: UnsignedEvent) -> anyhow::Result<Event> {
        let response = Self::make_rpc(&Nip70Request::SignEvent(event)).await?;
        if let Nip70Response::Event(event) = response {
            Ok(event)
        } else {
            anyhow::bail!("Unexpected response from server!")
        }
    }

    async fn get_relays(&self) -> anyhow::Result<Option<HashMap<String, RelayPolicy>>> {
        let response = Self::make_rpc(&Nip70Request::GetRelays).await?;
        if let Nip70Response::Relays(relays) = response {
            Ok(relays)
        } else {
            anyhow::bail!("Unexpected response from server!")
        }
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
    use std::time::Duration;

    use super::*;

    use nostr_sdk::{EventId, Keys, Kind, Timestamp};

    struct TestNip70Implementation {
        keys: Keys,
    }

    impl TestNip70Implementation {
        pub fn new_with_generated_keys() -> Self {
            Self {
                keys: Keys::generate(),
            }
        }
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
        let nip70 = Arc::from(TestNip70Implementation::new_with_generated_keys());
        let server = Nip70Server::new(nip70.clone()).unwrap();
        let client = Nip70Client::default();

        assert_eq!(
            nip70.get_public_key().await.unwrap(),
            client.get_public_key().await.unwrap()
        );
    }

    #[tokio::test]
    async fn sign_event_over_uds() {
        let nip70 = Arc::from(TestNip70Implementation::new_with_generated_keys());
        let server = Nip70Server::new(nip70.clone()).unwrap();
        let client = Nip70Client::default();

        let pubkey = client.get_public_key().await.unwrap();
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

        let event = client.sign_event(unsigned_event).await.unwrap();

        assert!(event.verify().is_ok());
    }

    #[tokio::test]
    async fn sign_event_over_uds_load() {
        let nip70 = Arc::from(TestNip70Implementation::new_with_generated_keys());
        let server = Nip70Server::new(nip70.clone()).unwrap();

        let mut client_handles = Vec::new();
        for i in 0..128 {
            let handle = tokio::spawn(async move {
                let client = Nip70Client::default();
                for j in 0..20 {
                    let pubkey = client.get_public_key().await.unwrap();
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

                    let event = client.sign_event(unsigned_event.clone()).await.unwrap();

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
}

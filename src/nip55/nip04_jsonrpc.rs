use crate::json_rpc::{JsonRpcRequest, JsonRpcResponse};
use nostr_sdk::{nips::nip04, Event, EventBuilder, Keys, Kind, PublicKey, Tag};
use serde::Serialize;
use serde_json::Value;

pub fn jsonrpc_request_to_nip04_encrypted_event(
    kind: Kind,
    request: &JsonRpcRequest,
    client_keypair: &Keys,
    server_pubkey: PublicKey,
) -> anyhow::Result<Event> {
    serialize_and_encrypt_nip04_event(kind, request, client_keypair, server_pubkey)
}

pub fn jsonrpc_response_to_nip04_encrypted_event(
    kind: Kind,
    response: &JsonRpcResponse,
    client_pubkey: PublicKey,
    server_keypair: &Keys,
) -> anyhow::Result<Event> {
    serialize_and_encrypt_nip04_event(kind, response, server_keypair, client_pubkey)
}

pub fn nip04_encrypted_event_to_jsonrpc_request(
    event: &Event,
    server_keypair: &Keys,
) -> anyhow::Result<JsonRpcRequest> {
    let mut jsonrpc_request_json_object: serde_json::Map<String, Value> =
        decrypt_and_deserialize_nip04_event(event, server_keypair)?;

    // Requests might not have the `id` field, but a valid JSON-RPC 2.0 request object should have it.
    // If it's missing, we need to add it before deserializing to a `JsonRpcRequest`.
    if !jsonrpc_request_json_object.contains_key("jsonrpc") {
        jsonrpc_request_json_object.insert("jsonrpc".to_string(), "2.0".into());
    }

    let request: JsonRpcRequest =
        serde_json::from_value(Value::Object(jsonrpc_request_json_object))?;

    Ok(request)
}

pub fn nip04_encrypted_event_to_jsonrpc_response(
    event: &Event,
    client_keypair: &Keys,
) -> anyhow::Result<JsonRpcResponse> {
    let mut jsonrpc_response_json_object: serde_json::Map<String, Value> =
        decrypt_and_deserialize_nip04_event(event, client_keypair)?;

    // Responses might not have the `id` field, but a valid JSON-RPC 2.0 response object should have it.
    // If it's missing, we need to add it before deserializing to a `JsonRpcResponse`.
    if !jsonrpc_response_json_object.contains_key("jsonrpc") {
        jsonrpc_response_json_object.insert("jsonrpc".to_string(), "2.0".into());
    }

    let response: JsonRpcResponse =
        serde_json::from_value(Value::Object(jsonrpc_response_json_object))?;

    Ok(response)
}

fn serialize_and_encrypt_nip04_event<T>(
    kind: Kind,
    data: &T,
    sender_keypair: &Keys,
    recipient_pubkey: PublicKey,
) -> anyhow::Result<Event>
where
    T: Serialize,
{
    Ok(EventBuilder::new(
        kind,
        nip04::encrypt(
            sender_keypair.secret_key()?,
            &recipient_pubkey,
            serde_json::to_string(data)?,
        )?,
        [Tag::public_key(recipient_pubkey)],
    )
    .to_event(sender_keypair)?)
}

fn decrypt_and_deserialize_nip04_event<T>(
    event: &Event,
    recipient_keypair: &Keys,
) -> anyhow::Result<T>
where
    T: serde::de::DeserializeOwned,
{
    event.verify()?;

    let decrypted_data = nip04::decrypt(
        recipient_keypair.secret_key()?,
        &event.pubkey,
        &event.content,
    )?;

    Ok(serde_json::from_str(&decrypted_data)?)
}

// TODO: More thoroughly test this module. Specifically error/edge cases.
// Also test an exact serialized string.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json_rpc::{JsonRpcId, JsonRpcResponseData, JsonRpcStructuredValue};
    use serde_json::json;

    #[test]
    fn test_jsonrpc_request_encryption_decryption_success() {
        let request = JsonRpcRequest::new(
            "test_method".to_string(),
            Some(JsonRpcStructuredValue::Array(vec![
                json!(1),
                json!(2),
                json!(3),
            ])),
            JsonRpcId::String("test_id".to_string()),
        );

        let server_keypair = Keys::generate();
        let server_pubkey = server_keypair.public_key();

        let client_keypair = Keys::generate();

        let event = jsonrpc_request_to_nip04_encrypted_event(
            Kind::NostrConnect,
            &request,
            &client_keypair,
            server_pubkey,
        )
        .expect("Failed to convert JSON-RPC request to NIP-04 encrypted event.");

        assert_eq!(event.kind, Kind::NostrConnect);
        assert_eq!(event.tags(), &[Tag::public_key(server_pubkey)]);

        let decrypted_request = nip04_encrypted_event_to_jsonrpc_request(&event, &server_keypair)
            .expect("Failed to convert NIP-04 encrypted event to JSON-RPC request.");

        assert_eq!(decrypted_request, request);
    }

    #[test]
    fn test_jsonrpc_response_encryption_decryption_success() {
        let response = JsonRpcResponse::new(
            JsonRpcResponseData::Success {
                result: Value::Array(vec![json!(1), json!(2), json!(3)]),
            },
            JsonRpcId::String("test_id".to_string()),
        );

        let server_keypair = Keys::generate();

        let client_keypair = Keys::generate();
        let client_pubkey = client_keypair.public_key();

        let event = jsonrpc_response_to_nip04_encrypted_event(
            Kind::NostrConnect,
            &response,
            client_pubkey,
            &server_keypair,
        )
        .expect("Failed to convert JSON-RPC response to NIP-04 encrypted event.");

        assert_eq!(event.kind, Kind::NostrConnect);
        assert_eq!(event.tags(), &[Tag::public_key(client_pubkey)]);

        let decrypted_response = nip04_encrypted_event_to_jsonrpc_response(&event, &client_keypair)
            .expect("Failed to convert NIP-04 encrypted event to JSON-RPC response.");

        assert_eq!(decrypted_response, response);
    }
}

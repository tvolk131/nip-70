pub mod uds;

use futures::StreamExt;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub trait JsonRpcServerTransport:
    futures::Stream<
        Item = (
            JsonRpcRequest,
            futures::channel::oneshot::Sender<JsonRpcResponse>,
        ),
    > + Unpin
{
}

#[async_trait::async_trait]
pub trait JsonRpcServerHandler {
    async fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let request_id = request.id().clone();
        let mut responses = self.handle_batch_request(vec![request]).await;

        if responses.len() != 1 {
            return JsonRpcResponse::new(
                JsonRpcResponseData::Error {
                    error: JsonRpcError {
                        code: JsonRpcErrorCode::InternalError,
                        message: format!(
                            "Internal error: Batch handler returned {} responses instead of 1",
                            responses.len()
                        ),
                        data: None,
                    },
                },
                request_id,
            );
        }

        // Unwrap is safe because we just checked that the length is 1.
        responses.pop().unwrap()
    }

    async fn handle_batch_request(&self, requests: Vec<JsonRpcRequest>) -> Vec<JsonRpcResponse>;
}

pub struct JsonRpcServer {
    task_handle: tokio::task::JoinHandle<()>,
}

impl JsonRpcServer {
    pub fn new(
        mut transport: Box<dyn JsonRpcServerTransport + Send + Sync>,
        handler: Box<dyn JsonRpcServerHandler + Send + Sync>,
    ) -> Self {
        let task_handle = tokio::spawn(async move {
            while let Some((request, response_sender)) = transport.next().await {
                let response = handler.handle_request(request).await;
                response_sender.send(response).unwrap();
            }
        });

        Self { task_handle }
    }

    pub fn stop(self) {
        // Drop the server, which will abort the task.
        drop(self);
    }
}

impl std::ops::Drop for JsonRpcServer {
    fn drop(&mut self) {
        // Abort the task, since it will loop forever otherwise.
        self.task_handle.abort();
    }
}

pub trait JsonRpcClientTransport<E> {
    async fn send_request(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse, E>;

    async fn send_batch_request(
        &self,
        requests: Vec<JsonRpcRequest>,
    ) -> Result<Vec<JsonRpcResponse>, E>;
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<JsonRpcStructuredValue>,
    id: JsonRpcId,
}

#[derive(PartialEq, Debug, Clone)]
pub enum JsonRpcId {
    Number(i32),
    String(String),
    Null,
}

impl JsonRpcId {
    fn to_json_value(&self) -> serde_json::Value {
        match self {
            JsonRpcId::Number(n) => serde_json::Value::Number((*n).into()),
            JsonRpcId::String(s) => serde_json::Value::String(s.clone()),
            JsonRpcId::Null => serde_json::Value::Null,
        }
    }
}

impl serde::Serialize for JsonRpcId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_json_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for JsonRpcId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        serde_json::Value::deserialize(deserializer).and_then(|value| {
            if value.is_i64() {
                Ok(JsonRpcId::Number(value.as_i64().unwrap() as i32))
            } else if value.is_string() {
                Ok(JsonRpcId::String(value.as_str().unwrap().to_string()))
            } else if value.is_null() {
                Ok(JsonRpcId::Null)
            } else {
                Err(serde::de::Error::custom("Invalid JSON-RPC ID"))
            }
        })
    }
}

impl JsonRpcRequest {
    pub fn new(method: String, params: Option<JsonRpcStructuredValue>, id: JsonRpcId) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method,
            params,
            id,
        }
    }

    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn params(&self) -> Option<&JsonRpcStructuredValue> {
        self.params.as_ref()
    }

    pub fn id(&self) -> &JsonRpcId {
        &self.id
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
#[serde(untagged)]
pub enum JsonRpcStructuredValue {
    Object(serde_json::Map<String, serde_json::Value>),
    Array(Vec<serde_json::Value>),
}

impl JsonRpcStructuredValue {
    pub fn into_value(self) -> serde_json::Value {
        match self {
            JsonRpcStructuredValue::Object(object) => serde_json::Value::Object(object),
            JsonRpcStructuredValue::Array(array) => serde_json::Value::Array(array),
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(flatten)]
    data: JsonRpcResponseData,
    id: JsonRpcId,
}

impl JsonRpcResponse {
    pub fn new(data: JsonRpcResponseData, id: JsonRpcId) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            data,
            id,
        }
    }

    pub fn data(&self) -> &JsonRpcResponseData {
        &self.data
    }

    pub fn id(&self) -> &JsonRpcId {
        &self.id
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[serde(untagged)]
pub enum JsonRpcResponseData {
    Success { result: serde_json::Value },
    Error { error: JsonRpcError },
}

// TODO: Make these fields private.
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct JsonRpcError {
    pub code: JsonRpcErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(PartialEq, Debug, Clone)]
pub enum JsonRpcErrorCode {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    InternalError,
    Custom(i32), // TODO: Make it so that this can only be used for custom error codes, not the standard ones above.
}

impl Serialize for JsonRpcErrorCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let code = match *self {
            JsonRpcErrorCode::ParseError => -32700,
            JsonRpcErrorCode::InvalidRequest => -32600,
            JsonRpcErrorCode::MethodNotFound => -32601,
            JsonRpcErrorCode::InvalidParams => -32602,
            JsonRpcErrorCode::InternalError => -32603,
            JsonRpcErrorCode::Custom(c) => c,
        };
        serializer.serialize_i32(code)
    }
}

impl<'de> Deserialize<'de> for JsonRpcErrorCode {
    fn deserialize<D>(deserializer: D) -> Result<JsonRpcErrorCode, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let code = i32::deserialize(deserializer)?;
        match code {
            -32700 => Ok(JsonRpcErrorCode::ParseError),
            -32600 => Ok(JsonRpcErrorCode::InvalidRequest),
            -32601 => Ok(JsonRpcErrorCode::MethodNotFound),
            -32602 => Ok(JsonRpcErrorCode::InvalidParams),
            -32603 => Ok(JsonRpcErrorCode::InternalError),
            _ => Ok(JsonRpcErrorCode::Custom(code)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_and_deserialize_json_rpc_request_with_empty_params() {
        let request = JsonRpcRequest::new("get_public_key".to_string(), None, JsonRpcId::Null);
        let request_json_string = serde_json::to_string(&request).unwrap();

        assert_eq!(
            request_json_string,
            "{\"jsonrpc\":\"2.0\",\"method\":\"get_public_key\",\"id\":null}"
        );

        let parsed_request: JsonRpcRequest = serde_json::from_str(&request_json_string).unwrap();

        assert_eq!(parsed_request, request);
    }

    #[test]
    fn serialize_and_deserialize_json_rpc_request_with_object_params() {
        let request = JsonRpcRequest::new(
            "get_public_key".to_string(),
            Some(JsonRpcStructuredValue::Object(
                serde_json::from_str("{\"key_type\":\"rsa\"}").unwrap(),
            )),
            JsonRpcId::Null,
        );
        let request_json_string = serde_json::to_string(&request).unwrap();

        assert_eq!(
            request_json_string,
            "{\"jsonrpc\":\"2.0\",\"method\":\"get_public_key\",\"params\":{\"key_type\":\"rsa\"},\"id\":null}"
        );

        let parsed_request: JsonRpcRequest = serde_json::from_str(&request_json_string).unwrap();

        assert_eq!(parsed_request, request);
    }

    #[test]
    fn serialize_and_deserialize_json_rpc_request_with_array_params() {
        let request = JsonRpcRequest::new(
            "get_public_key".to_string(),
            Some(JsonRpcStructuredValue::Array(vec![
                serde_json::from_str("1").unwrap(),
                serde_json::from_str("2").unwrap(),
                serde_json::from_str("3").unwrap(),
            ])),
            JsonRpcId::Null,
        );
        let request_json_string = serde_json::to_string(&request).unwrap();

        assert_eq!(
            request_json_string,
            "{\"jsonrpc\":\"2.0\",\"method\":\"get_public_key\",\"params\":[1,2,3],\"id\":null}"
        );

        let parsed_request: JsonRpcRequest = serde_json::from_str(&request_json_string).unwrap();

        assert_eq!(parsed_request, request);
    }

    #[test]
    fn serialize_and_deserialize_json_rpc_response_with_result() {
        let response = JsonRpcResponse::new(
            JsonRpcResponseData::Success {
                result: serde_json::from_str("\"foo\"").unwrap(),
            },
            JsonRpcId::Null,
        );
        let response_json_string = serde_json::to_string(&response).unwrap();

        assert_eq!(
            response_json_string,
            "{\"jsonrpc\":\"2.0\",\"result\":\"foo\",\"id\":null}"
        );

        let parsed_response: JsonRpcResponse = serde_json::from_str(&response_json_string).unwrap();

        assert_eq!(parsed_response, response);
    }

    #[test]
    fn serialize_and_deserialize_json_rpc_response_with_error_no_data() {
        let response = JsonRpcResponse::new(
            JsonRpcResponseData::Error {
                error: JsonRpcError {
                    code: JsonRpcErrorCode::InternalError,
                    message: "foo".to_string(),
                    data: None,
                },
            },
            JsonRpcId::Null,
        );
        let response_json_string = serde_json::to_string(&response).unwrap();

        assert_eq!(
            response_json_string,
            "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32603,\"message\":\"foo\"},\"id\":null}"
        );

        let parsed_response: JsonRpcResponse = serde_json::from_str(&response_json_string).unwrap();

        assert_eq!(parsed_response, response);
    }

    #[test]
    fn serialize_and_deserialize_json_rpc_response_with_error_with_data() {
        let response = JsonRpcResponse::new(
            JsonRpcResponseData::Error {
                error: JsonRpcError {
                    code: JsonRpcErrorCode::InternalError,
                    message: "foo".to_string(),
                    data: Some(serde_json::from_str("\"bar\"").unwrap()),
                },
            },
            JsonRpcId::Null,
        );
        let response_json_string = serde_json::to_string(&response).unwrap();

        assert_eq!(
            response_json_string,
            "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32603,\"message\":\"foo\",\"data\":\"bar\"},\"id\":null}"
        );

        let parsed_response: JsonRpcResponse = serde_json::from_str(&response_json_string).unwrap();

        assert_eq!(parsed_response, response);
    }

    #[test]
    fn serialize_and_deserialize_integer_id() {
        let id = JsonRpcId::Number(1234);
        let id_json_string = serde_json::to_string(&id).unwrap();

        assert_eq!(id_json_string, "1234");

        let parsed_id: JsonRpcId = serde_json::from_str(&id_json_string).unwrap();

        assert_eq!(parsed_id, id);
    }

    #[test]
    fn serialize_and_deserialize_string_id() {
        let id = JsonRpcId::String("foo".to_string());
        let id_json_string = serde_json::to_string(&id).unwrap();

        assert_eq!(id_json_string, "\"foo\"");

        let parsed_id: JsonRpcId = serde_json::from_str(&id_json_string).unwrap();

        assert_eq!(parsed_id, id);
    }

    #[test]
    fn serialize_and_deserialize_null_id() {
        let id = JsonRpcId::Null;
        let id_json_string = serde_json::to_string(&id).unwrap();

        assert_eq!(id_json_string, "null");

        let parsed_id: JsonRpcId = serde_json::from_str(&id_json_string).unwrap();

        assert_eq!(parsed_id, id);
    }
}

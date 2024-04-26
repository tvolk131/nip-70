use super::super::{JsonRpcClientTransport, JsonRpcRequest, JsonRpcResponse};
use crate::{
    json_rpc::{JsonRpcError, JsonRpcErrorCode, JsonRpcId, JsonRpcResponseData},
    uds_req_res::{
        client::{UdsClientError, UnixDomainSocketClientTransport},
        UdsRequest, UdsResponse,
    },
};

#[derive(Clone)]
pub struct UnixDomainSocketJsonRpcClientTransport {
    uds_client_transport: UnixDomainSocketClientTransport,
}

impl UnixDomainSocketJsonRpcClientTransport {
    pub fn new(uds_address: String) -> Self {
        Self {
            uds_client_transport: UnixDomainSocketClientTransport::new(uds_address),
        }
    }
}

impl JsonRpcClientTransport<UdsClientError> for UnixDomainSocketJsonRpcClientTransport {
    async fn send_request(
        &self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, UdsClientError> {
        let json_request = serde_json::to_value(request).unwrap();
        let json_response = self.uds_client_transport.send_request(json_request).await?;
        Ok(serde_json::from_value(json_response).unwrap())
    }

    async fn send_batch_request(
        &self,
        _requests: Vec<JsonRpcRequest>,
    ) -> Result<Vec<JsonRpcResponse>, UdsClientError> {
        unimplemented!("Batch requests are not supported yet.")

        // let json_request_array = serde_json::to_value(requests).unwrap();
        // let json_response_array = self
        //     .uds_client_transport
        //     .send_request(json_request_array)
        //     .await?;
        // Ok(serde_json::from_value(json_response_array).unwrap())
    }
}

impl UdsRequest for serde_json::Value {}

impl UdsResponse for serde_json::Value {
    fn request_parse_error_response() -> Self {
        serde_json::to_value(JsonRpcResponse::new(
            JsonRpcResponseData::Error {
                error: JsonRpcError {
                    code: JsonRpcErrorCode::ParseError,
                    message: "Failed to parse JSON-RPC request.".to_string(),
                    data: None,
                },
            },
            JsonRpcId::Null,
        ))
        .unwrap()
    }

    fn internal_error_response(msg: String) -> Self {
        serde_json::to_value(JsonRpcResponse::new(
            JsonRpcResponseData::Error {
                error: JsonRpcError {
                    code: JsonRpcErrorCode::InternalError,
                    message: msg,
                    data: None,
                },
            },
            JsonRpcId::Null,
        ))
        .unwrap()
    }
}

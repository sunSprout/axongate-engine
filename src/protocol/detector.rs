use crate::error::Result;
use crate::models::ClientProtocol;
use axum::body::{Body, Bytes};
use axum::http::Request;
use serde_json::Value;

pub struct ProtocolDetector;

impl ProtocolDetector {
    // 判断客户端请求协议
    pub fn detect_from_request(req: &Request<Body>) -> Result<ClientProtocol> {
        // 根据路径判断
        let path = req.uri().path();

        if path.starts_with("/v1/chat/completions") {
            return Ok(ClientProtocol::OpenAI);
        }

        if path.starts_with("/v1/messages") {
            return Ok(ClientProtocol::Anthropic);
        }

        // 支持 /v1/responses 路径，识别为 OpenAI 协议
        if path.starts_with("/v1/responses") {
            return Ok(ClientProtocol::OpenAI);
        }

        // 根据 Authorization header 判断
        if let Some(auth) = req.headers().get("authorization") {
            if let Ok(auth_str) = auth.to_str() {
                if auth_str.starts_with("Bearer sk-") {
                    return Ok(ClientProtocol::OpenAI);
                }
                if auth_str.starts_with("x-api-key") {
                    return Ok(ClientProtocol::Anthropic);
                }
            }
        }

        // 默认为 OpenAI
        Ok(ClientProtocol::OpenAI)
    }

    // 判断是否是流式请求
    pub fn is_stream_request(req: &Bytes) -> bool {
        // 尝试将请求体解析为 JSON
        let json: Value = match serde_json::from_slice(req) {
            Ok(v) => v,
            Err(_) => return false,
        };
        
        // 获取 stream 字段并判断是否为 true
        json.get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
}

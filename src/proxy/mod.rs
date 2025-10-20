use crate::config::ProxyConfig;
use crate::error::{Error, Result};
use crate::models::RouteConfig;
use bytes::Bytes;
use futures::StreamExt;
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue},
    Client, Response,
};
use tracing::{error, info};

pub struct ProxyForwarder {
    client: Client,
    // Dedicated client for streaming (no global timeout)
    streaming_client: Client,
}

impl ProxyForwarder {
    pub fn new(config: ProxyConfig) -> Result<Self> {
        // Standard client: obeys configured request timeout
        let client = Client::builder()
            .timeout(config.timeout)
            .pool_max_idle_per_host(config.max_connections)
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .tcp_keepalive(if config.keep_alive {
                Some(std::time::Duration::from_secs(30))
            } else {
                None
            })
            .build()
            .map_err(|e| Error::Http(e))?;

        // Streaming client: no global request timeout to allow long-lived SSE
        let streaming_client = Client::builder()
            .pool_max_idle_per_host(config.max_connections)
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .tcp_keepalive(if config.keep_alive {
                Some(std::time::Duration::from_secs(30))
            } else {
                None
            })
            .build()
            .map_err(|e| Error::Http(e))?;

        Ok(Self { client, streaming_client })
    }

    pub async fn forward_request(
        &self,
        route_config: &RouteConfig,
        request_body: Bytes,
        custom_path: Option<&str>,
        client_headers: &HeaderMap,
    ) -> Result<Bytes> {
        // 直接做请求转换
        let result = self.send_request(route_config, request_body.clone(), custom_path, client_headers).await;

        match result {
            Ok(response) => {
                // 转换成功，发送请求
                return self.process_response(response).await;
            }
            Err(e) => {
                return Err(e);
            }
        }
    }

    async fn send_request(
        &self,
        route_config: &RouteConfig,
        request_body: Bytes,
        custom_path: Option<&str>,
        client_headers: &HeaderMap,
    ) -> Result<Response> {
        info!("send_request: start -> {}", route_config.api_endpoint);

        // 先复制客户端headers（已过滤敏感header）
        let mut headers = client_headers.clone();

        // 根据目标协议设置正确的认证header
        match &route_config.protocol {
            crate::models::TargetProtocol::Anthropic => {
                // Anthropic使用 x-api-key 认证
                headers.insert(
                    HeaderName::from_static("x-api-key"),
                    HeaderValue::from_str(&route_config.token)
                        .map_err(|_| Error::Proxy("Invalid token format".into()))?,
                );
            }
            crate::models::TargetProtocol::OpenAI | crate::models::TargetProtocol::Custom(_) => {
                // OpenAI和自定义协议使用 Authorization: Bearer
                headers.insert(
                    HeaderName::from_static("authorization"),
                    HeaderValue::from_str(&format!("Bearer {}", route_config.token))
                        .map_err(|_| Error::Proxy("Invalid token format".into()))?,
                );
            }
        }

        // 确保content-type存在
        headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );

        // 处理 API endpoint，移除尾部斜杠
        let base_url = route_config.api_endpoint.trim_end_matches('/');

        // 根据 custom_path 或协议选择正确的 API 路径，智能处理 /v1 前缀
        let api_path = if let Some(path) = custom_path {
            // 如果指定了自定义路径（如 /v1/responses）
            if path == "/v1/responses" {
                if base_url.ends_with("/v1") {
                    "/responses" // 已有 /v1，只添加后续路径
                } else {
                    "/v1/responses" // 没有 /v1，添加完整路径
                }
            } else {
                // 其他自定义路径直接使用
                path
            }
        } else {
            // 根据协议选择正确的 API 路径
            match &route_config.protocol {
                crate::models::TargetProtocol::OpenAI => {
                    if base_url.ends_with("/v1") {
                        "/chat/completions" // 已有 /v1，只添加后续路径
                    } else {
                        "/v1/chat/completions" // 没有 /v1，添加完整路径
                    }
                }
                crate::models::TargetProtocol::Anthropic => {
                    if base_url.ends_with("/v1") {
                        "/messages"
                    } else {
                        "/v1/messages"
                    }
                }
                crate::models::TargetProtocol::Custom(_) => {
                    if base_url.ends_with("/v1") {
                        "/chat/completions"
                    } else {
                        "/v1/chat/completions"
                    }
                }
            }
        };

        let url = format!("{}{}", base_url, api_path);

        // request building logs removed to reduce noise
        let response = self
            .client
            .post(&url)
            .headers(headers)
            .body(request_body)
            .send()
            .await
            .map_err(|e| {
                error!("HTTP client connection failed: {:?}", e);
                Error::Http(e)
            })?;

        Ok(response)
    }

    // Same as send_request but using the streaming client (no global timeout)
    async fn send_request_stream(
        &self,
        route_config: &RouteConfig,
        request_body: Bytes,
        custom_path: Option<&str>,
        client_headers: &HeaderMap,
    ) -> Result<Response> {
        info!(
            "send_request_stream: start -> {}",
            route_config.api_endpoint
        );

        // 先复制客户端headers（已过滤敏感header）
        let mut headers = client_headers.clone();

        // 根据目标协议设置正确的认证header
        match &route_config.protocol {
            crate::models::TargetProtocol::Anthropic => {
                // Anthropic使用 x-api-key 认证
                headers.insert(
                    HeaderName::from_static("x-api-key"),
                    HeaderValue::from_str(&route_config.token)
                        .map_err(|_| Error::Proxy("Invalid token format".into()))?,
                );
            }
            crate::models::TargetProtocol::OpenAI | crate::models::TargetProtocol::Custom(_) => {
                // OpenAI和自定义协议使用 Authorization: Bearer
                headers.insert(
                    HeaderName::from_static("authorization"),
                    HeaderValue::from_str(&format!("Bearer {}", route_config.token))
                        .map_err(|_| Error::Proxy("Invalid token format".into()))?,
                );
            }
        }

        // 确保content-type存在
        headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );

        let base_url = route_config.api_endpoint.trim_end_matches('/');

        // 根据 custom_path 或协议选择正确的 API 路径，智能处理 /v1 前缀
        let api_path = if let Some(path) = custom_path {
            // 如果指定了自定义路径（如 /v1/responses）
            if path == "/v1/responses" {
                if base_url.ends_with("/v1") {
                    "/responses" // 已有 /v1，只添加后续路径
                } else {
                    "/v1/responses" // 没有 /v1，添加完整路径
                }
            } else {
                // 其他自定义路径直接使用
                path
            }
        } else {
            // 根据协议选择正确的 API 路径
            match &route_config.protocol {
                crate::models::TargetProtocol::OpenAI => {
                    if base_url.ends_with("/v1") { "/chat/completions" } else { "/v1/chat/completions" }
                }
                crate::models::TargetProtocol::Anthropic => {
                    if base_url.ends_with("/v1") { "/messages" } else { "/v1/messages" }
                }
                crate::models::TargetProtocol::Custom(_) => {
                    if base_url.ends_with("/v1") { "/chat/completions" } else { "/v1/chat/completions" }
                }
            }
        };

        let url = format!("{}{}", base_url, api_path);

        // request building logs removed to reduce noise
        let response = self
            .streaming_client
            .post(&url)
            .headers(headers)
            .body(request_body)
            .send()
            .await
            .map_err(|e| {
                error!("HTTP client connection failed (stream): {:?}", e);
                Error::Http(e)
            })?;

        Ok(response)
    }

    // 处理非流式响应
    async fn process_response(&self, response: Response) -> Result<Bytes> {
        let status = response.status();
        if !status.is_success() {
            let body = response
                .bytes()
                .await
                .unwrap_or_else(|_| Bytes::from("Failed to read error response"));

            error!(
                "Upstream error response (status {}): {}",
                status,
                String::from_utf8_lossy(&body)
            );

            return Err(Error::Proxy(format!(
                "Upstream returned error status {}: {}",
                status,
                String::from_utf8_lossy(&body)
            )));
        }

        info!("Upstream success response status: {}", status);
        let body = response.bytes().await.map_err(|e| Error::Http(e))?;
        Ok(body)
    }

    pub fn is_client_error(&self, error: &Error) -> bool {
        match error {
            Error::Proxy(msg) => {
                // 4xx错误，客户端错误，不应重试
                msg.contains("400")
                    || msg.contains("401")
                    || msg.contains("403")
                    || msg.contains("404")
                    || msg.contains("422")
                    || msg.contains("429")
            }
            _ => false,
        }
    }

    /// 新的纯粹流式接口，返回字节流而不包含 Axum 依赖
    /// 这是架构重构第一步的核心接口
    pub async fn stream(
        &self,
        route_config: &RouteConfig,
        request_body: Bytes,
        custom_path: Option<&str>,
        client_headers: &HeaderMap,
    ) -> Result<impl futures::Stream<Item = Result<Bytes>>> {
        info!("stream: start");
        // Use streaming client without global timeout
        let response = self.send_request_stream(route_config, request_body, custom_path, client_headers).await?;
        // request sent successfully

        let status = response.status();
        if !status.is_success() {
            let body = response
                .bytes()
                .await
                .unwrap_or_else(|_| Bytes::from("Failed to read error response"));

            error!(
                "Upstream stream error response (status {}): {}",
                status,
                String::from_utf8_lossy(&body)
            );

            return Err(Error::Proxy(format!(
                "Upstream returned error status {}: {}",
                status,
                String::from_utf8_lossy(&body)
            )));
        }

        // 返回纯粹的字节流，不包含任何框架依赖
        info!("stream: established (status {})", status);
        let stream = response.bytes_stream().map(move |chunk| {
            match chunk {
                Ok(bytes) => Ok(Bytes::from(bytes)),
                Err(e) => Err(Error::Http(e))
            }
        });
        info!("stream: ready to yield");
        Ok(stream)
    }

    #[deprecated(note = "Use `stream` method instead. This will be removed in future versions.")]
    pub async fn forward_stream(
        &self,
        route_config: &RouteConfig,
        request_body: Bytes,
    ) -> Result<impl futures::Stream<Item = Result<Bytes>>> {
        // 兼容性：调用新的 stream 接口，不使用 custom_path，使用空的client_headers
        let empty_headers = HeaderMap::new();
        self.stream(route_config, request_body, None, &empty_headers).await
    }
}

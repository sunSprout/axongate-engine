use ai_gateway_engine::{
    cache::Cache,
    config::Config,
    error::Error,
    models::{ClientProtocol, ErrorEvent, RouteConfig, UsageEvent},
    protocol::{adapter::UniversalAdapter, detector::ProtocolDetector, ProtocolAdapter},
    proxy::ProxyForwarder,
    router::Router,
    telemetry::TelemetryModule,
    usage_collector::StreamUsageCollector,
    Result,
};
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{Request, Response, StatusCode},
    routing::{get, post},
    Router as AxumRouter,
};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use tower_http::trace::TraceLayer;
use tracing::{debug, error, info};
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    router: Arc<Router>,
    proxy: Arc<ProxyForwarder>,
    adapter: Arc<UniversalAdapter>,
    telemetry: Arc<TelemetryModule>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志，支持通过环境变量配置，默认info级别
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    info!("Starting AI Gateway Engine...");

    // 加载配置
    let config = Config::from_file("config.yaml").unwrap_or_else(|_| {
        info!("Failed to load config.yaml, using default config");
        Config::default()
    });

    // 初始化各模块
    let cache = Arc::new(Cache::new(config.cache.ttl, config.cache.max_lifetime));
    let router = Arc::new(Router::new(cache.clone(), config.business_api.clone())?);
    let proxy = Arc::new(ProxyForwarder::new(config.proxy.clone())?);
    let adapter = Arc::new(UniversalAdapter::new());
    let telemetry = Arc::new(TelemetryModule::new(config.business_api.base_url.clone())?);

    let state = AppState {
        router,
        proxy,
        adapter,
        telemetry,
    };

    // 创建路由
    let app = AxumRouter::new()
        .route("/health", get(health))
        .route("/v1/chat/completions", post(handle_request))
        .route("/v1/messages", post(handle_request))
        .route("/v1/responses", post(handle_request))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request<Body>| {
                // 过滤掉健康检查的日志
                if request.uri().path() == "/health" {
                    tracing::trace_span!("health_check")
                } else {
                    tracing::info_span!(
                        "request",
                        method = %request.method(),
                        uri = %request.uri(),
                        version = ?request.version(),
                    )
                }
            }),
        )
        .with_state(state);

    // 启动服务器
    let addr = format!("{}:{}", config.server.host, config.server.port);
    info!("Server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();

    Ok(())
}

async fn health() -> Response<Body> {
    let body = serde_json::json!({
        "status": "healthy"
    })
    .to_string();
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

async fn handle_request(State(state): State<AppState>, req: Request<Body>) -> Response<Body> {
    // 提取请求路径
    let request_path = req.uri().path().to_string();

    // 检测客户端协议
    let client_protocol = match ProtocolDetector::detect_from_request(&req) {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to detect protocol: {}", e);
            return error_response(StatusCode::BAD_REQUEST, "Invalid request format");
        }
    };

    // 提取认证信息
    let user_token = match extract_token(&req) {
        Some(token) => token,
        None => {
            return error_response(StatusCode::UNAUTHORIZED, "Missing authorization");
        }
    };

    // 提取客户端headers（排除拦截列表）
    let client_headers = filter_client_headers(&req);

    // 读取请求体
    let body_bytes = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
        Ok(bytes) => bytes,
        Err(e) => {
            error!("Failed to read request body: {}", e);
            return error_response(StatusCode::BAD_REQUEST, "Invalid request body");
        }
    };

    // 打印客户端请求日志
    let token_display = if user_token.len() > 8 {
        format!(
            "{}...{}",
            &user_token[..4],
            &user_token[user_token.len() - 4..]
        )
    } else {
        "***".to_string()
    };

    // 解析请求获取模型名
    let requested_model = match extract_model(&body_bytes) {
        Some(model) => model,
        None => {
            return error_response(StatusCode::BAD_REQUEST, "Missing model field");
        }
    };

    info!(
        "Request received - protocol: {:?}, model: {}, path: {}",
        client_protocol, requested_model, request_path
    );

    // 获取路由配置
    let route_configs = match state
        .router
        .resolve_route(&user_token, &requested_model)
        .await
    {
        Ok(configs) => configs,
        Err(e) => {
            error!("Failed to resolve route: {}", e);
            return error_response(StatusCode::SERVICE_UNAVAILABLE, "No available routes");
        }
    };

    // 判断是否是流式请求
    let is_stream = ProtocolDetector::is_stream_request(&body_bytes);

    info!(
        "Request routing - stream: {}, protocol: {:?}, model: {}, path: {}",
        is_stream, client_protocol, requested_model, request_path
    );

    if is_stream {
        handle_stream(
            state,
            route_configs,
            client_protocol,
            body_bytes,
            user_token,
            requested_model,
            request_path,
            client_headers,
        )
        .await
    } else {
        handle_non_stream(
            state,
            route_configs,
            client_protocol,
            body_bytes,
            user_token,
            requested_model,
            request_path,
            client_headers,
        )
        .await
    }
}

fn extract_token(req: &Request<Body>) -> Option<String> {
    req.headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

fn extract_model(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("model")?.as_str().map(|s| s.to_string())
}

fn filter_client_headers(req: &Request<Body>) -> reqwest::header::HeaderMap {
    let mut filtered = reqwest::header::HeaderMap::new();

    // 拦截列表：这些header不应该转发到上游
    let blocked_headers = [
        "authorization",     // 需要根据protocol重写
        "host",              // 指向目标endpoint
        "content-length",    // reqwest自动计算
        "transfer-encoding", // 避免冲突
        "connection",        // 避免冲突
    ];

    for (name, value) in req.headers().iter() {
        let name_str = name.as_str().to_lowercase();
        if !blocked_headers.contains(&name_str.as_str()) {
            // 将axum的HeaderName/HeaderValue转换为reqwest的类型
            if let Ok(reqwest_name) =
                reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes())
            {
                if let Ok(reqwest_value) =
                    reqwest::header::HeaderValue::from_bytes(value.as_bytes())
                {
                    filtered.insert(reqwest_name, reqwest_value);
                }
            }
        }
    }

    filtered
}

// 从响应中提取usage信息
fn extract_usage_from_response(
    protocol: &ai_gateway_engine::models::TargetProtocol,
    body: &[u8],
) -> Option<(i32, i32)> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let usage = v.get("usage")?;

    match protocol {
        ai_gateway_engine::models::TargetProtocol::OpenAI
        | ai_gateway_engine::models::TargetProtocol::Custom(_) => {
            // OpenAI格式: { "prompt_tokens": N, "completion_tokens": M }
            let input = usage.get("prompt_tokens")?.as_i64()? as i32;
            let output = usage.get("completion_tokens")?.as_i64()? as i32;
            Some((input, output))
        }
        ai_gateway_engine::models::TargetProtocol::Anthropic => {
            // Anthropic格式: { "input_tokens": N, "output_tokens": M }
            let input = usage.get("input_tokens")?.as_i64()? as i32;
            let output = usage.get("output_tokens")?.as_i64()? as i32;
            Some((input, output))
        }
    }
}

fn error_response(status: StatusCode, message: &str) -> Response<Body> {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "gateway_error",
        }
    });

    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn create_error_response(error: &Error) -> Response<Body> {
    match error {
        Error::Proxy(msg) => {
            // 解析上游错误信息
            if msg.contains("400") {
                // 提取上游的错误响应体
                if let Some(start) = msg.find(": ") {
                    let upstream_error = &msg[start + 2..];
                    return Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .header("content-type", "application/json")
                        .body(Body::from(upstream_error.to_string()))
                        .unwrap();
                }
                error_response(StatusCode::BAD_REQUEST, msg)
            } else if msg.contains("401") {
                error_response(StatusCode::UNAUTHORIZED, "Unauthorized")
            } else if msg.contains("403") {
                error_response(StatusCode::FORBIDDEN, "Forbidden")
            } else if msg.contains("404") {
                error_response(StatusCode::NOT_FOUND, "Not Found")
            } else if msg.contains("422") {
                error_response(StatusCode::UNPROCESSABLE_ENTITY, msg)
            } else if msg.contains("429") {
                error_response(StatusCode::TOO_MANY_REQUESTS, "Too Many Requests")
            } else {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        }
        _ => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

// 处理流式请求
// 架构重构后：Transport 层负责构建 Response，Proxy 层只返回纯粹的字节流
async fn handle_stream(
    state: AppState,
    route_configs: Vec<RouteConfig>,
    client_protocol: ClientProtocol,
    body_bytes: Bytes,
    user_token: String,
    requested_model: String,
    request_path: String,
    client_headers: reqwest::header::HeaderMap,
) -> Response<Body> {
    // 生成请求ID用于去重
    let request_id = Uuid::new_v4().to_string();

    // 判断是否需要自定义路径
    let custom_path = if request_path == "/v1/responses" {
        Some("/v1/responses")
    } else {
        None
    };

    // 尝试每个路由配置
    for (index, config) in route_configs.iter().enumerate() {
        let target_protocol = &config.protocol;

        // 将请求转换为目标协议格式
        let transformed_request = match state
            .adapter
            .transform_request(
                &client_protocol,
                target_protocol,
                &config.model,
                body_bytes.clone(),
            )
            .await
        {
            Ok(body) => body,
            Err(e) => {
                error!("Failed to transform request: {}", e);
                continue;
            }
        };

        // 使用新的 stream 接口获取纯粹的字节流
        match state
            .proxy
            .stream(&config, transformed_request, custom_path, &client_headers)
            .await
        {
            Ok(byte_stream) => {

                // 创建Usage收集器来收集流式响应的token使用情况（在协议转换前）
                let usage_collector = Arc::new(StreamUsageCollector::new(
                    request_id.clone(),
                    user_token.clone(),
                    config.clone(), // 传递完整的RouteConfig
                    state.telemetry.clone(),
                ));

                // 包装原始流以收集usage信息
                let wrapped_stream = usage_collector.wrap_stream(byte_stream).await;

                // 对流进行协议转换
                match state
                    .adapter
                    .transform_stream_chunk(target_protocol, &client_protocol, wrapped_stream)
                    .await
                {
                    Ok(transformed_stream) => {
                        // 在 Transport 层构建流式响应
                        // 设置 SSE 必要的响应头
                        let response = Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "text/event-stream")
                            .header("cache-control", "no-cache")
                            .header("connection", "keep-alive")
                            .header("x-accel-buffering", "no") // 禁用 nginx 缓冲
                            .body(Body::from_stream(transformed_stream))
                            .unwrap();

                        return response;
                    }
                    Err(e) => {
                        error!("Failed to transform stream: {}", e);
                        continue;
                    }
                }
            }
            Err(e) => {
                error!("Stream request failed for {}: {}", config.api_endpoint, e);

                // 上报错误
                state.telemetry.report_error(ErrorEvent {
                    token: config.token.clone(),
                    model: config.model.clone(),
                    api: config.api_endpoint.clone(),
                    msg: e.to_string(),
                    provider_token_id: Some(config.provider_token_id.clone()), // 添加provider_token_id
                });

                // 检查是否为客户端错误（4xx），如果是则直接返回
                if state.proxy.is_client_error(&e) {
                    return create_error_response(&e);
                }

                // 从缓存中移除失败的配置
                state
                    .router
                    .remove_failed_route(&user_token, &requested_model, &config)
                    .await;
                continue;
            }
        }
    }

    // 所有路由都失败
    error_response(StatusCode::SERVICE_UNAVAILABLE, "All stream routes failed")
}

// 处理非流式请求
// 非流式路径会等待上游请求完整完成：
// 1) 发送请求 -> 2) 读取完整响应体 -> 3) 做协议转换 -> 4) 一次性返回给客户端。
// 与流式不同，这里不会提前把响应返回给客户端，也没有持续推送的后台任务。
async fn handle_non_stream(
    state: AppState,
    route_configs: Vec<RouteConfig>,
    client_protocol: ClientProtocol,
    body_bytes: Bytes,
    user_token: String,
    requested_model: String,
    request_path: String,
    client_headers: reqwest::header::HeaderMap,
) -> Response<Body> {
    // 生成请求ID用于去重
    let request_id = Uuid::new_v4().to_string();

    // 判断是否需要自定义路径
    let custom_path = if request_path == "/v1/responses" {
        Some("/v1/responses")
    } else {
        None
    };

    // 尝试每个路由配置
    for config in route_configs {
        let target_protocol = &config.protocol;

        // 将请求转换为目标协议格式
        let transformed_request = match state
            .adapter
            .transform_request(
                &client_protocol,
                target_protocol,
                &config.model,
                body_bytes.clone(),
            )
            .await
        {
            Ok(body) => body,
            Err(e) => {
                error!("Failed to transform request: {}", e);
                continue;
            }
        };

        // 转发请求
        match state
            .proxy
            .forward_request(&config, transformed_request, custom_path, &client_headers)
            .await
        {
            Ok(response_body) => {
                // 立即提取并上报usage信息（无论后续转换是否成功）
                if let Some((input_tokens, output_tokens)) =
                    extract_usage_from_response(&target_protocol, &response_body)
                {
                    state.telemetry.report_usage(UsageEvent {
                        request_id: request_id.clone(),
                        token: user_token.clone(),
                        model: requested_model.clone(),
                        api: config.api_endpoint.clone(),
                        input_tokens,
                        output_tokens,
                        // 新增的ID字段
                        model_id: config.model_id.clone(),
                        provider_id: config.provider_id.clone(),
                        provider_token_id: config.provider_token_id.clone(),
                    });
                }

                // 验证响应体非空
                if response_body.is_empty() {
                    error!(
                        "Empty response body from upstream: endpoint={}, model={}, protocol={:?}",
                        config.api_endpoint, config.model, target_protocol
                    );
                    continue;
                }

                // 转换响应
                match state
                    .adapter
                    .transform_response(target_protocol, &client_protocol, response_body)
                    .await
                {
                    Ok(transformed) => {
                        return Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "application/json")
                            .body(Body::from(transformed))
                            .unwrap();
                    }
                    Err(e) => {
                        error!("Failed to transform response: {}", e);
                        continue;
                    }
                }
            }
            Err(e) => {
                error!("Request failed for {}: {}", config.api_endpoint, e);

                // 上报错误
                state.telemetry.report_error(ErrorEvent {
                    token: config.token.clone(),
                    model: config.model.clone(),
                    api: config.api_endpoint.clone(),
                    msg: e.to_string(),
                    provider_token_id: Some(config.provider_token_id.clone()), // 添加provider_token_id
                });

                // 检查是否为客户端错误（4xx），如果是则直接返回
                if state.proxy.is_client_error(&e) {
                    return create_error_response(&e);
                }

                // 从缓存中移除失败的配置
                state
                    .router
                    .remove_failed_route(&user_token, &requested_model, &config)
                    .await;
                continue;
            }
        }
    }

    // 所有路由都失败
    error_response(StatusCode::SERVICE_UNAVAILABLE, "All routes failed")
}

use std::sync::{Arc, Mutex};
use futures::Stream;
use futures::StreamExt;
use bytes::Bytes;
use tracing::debug;
use crate::models::{UsageEvent, TargetProtocol, RouteConfig};
use crate::telemetry::TelemetryModule;
use crate::Result;

/// 流式响应的Usage收集器
pub struct StreamUsageCollector {
    request_id: String,
    user_token: String,
    // 携带完整的RouteConfig，便于灵活上报
    route_config: RouteConfig,
    input_tokens: Arc<Mutex<Option<i32>>>,
    output_tokens: Arc<Mutex<Option<i32>>>,
    telemetry: Arc<TelemetryModule>,
    // 缓冲区用于累积跨多个chunks的SSE事件
    buffer: Arc<Mutex<String>>,
}

impl StreamUsageCollector {
    pub fn new(
        request_id: String,
        user_token: String,
        route_config: RouteConfig,
        telemetry: Arc<TelemetryModule>,
    ) -> Self {
        Self {
            request_id,
            user_token,
            route_config,
            input_tokens: Arc::new(Mutex::new(None)),
            output_tokens: Arc::new(Mutex::new(None)),
            telemetry,
            buffer: Arc::new(Mutex::new(String::new())),
        }
    }

    /// 处理流式响应chunk，提取usage信息
    pub fn process_chunk(&self, chunk: &[u8]) {
        // 将chunk转换为字符串并追加到缓冲区
        let chunk_str = match std::str::from_utf8(chunk) {
            Ok(s) => s,
            Err(e) => {
                debug!("Usage Collector - Invalid UTF-8 in chunk: {}", e);
                return;
            }
        };

        debug!("Usage Collector - Processing chunk ({} bytes)", chunk.len());

        // 追加到缓冲区
        let mut buffer = self.buffer.lock().unwrap();
        buffer.push_str(chunk_str);

        // 处理缓冲区中所有完整的SSE事件（以\n\n分隔）
        loop {
            // 查找事件分隔符 \n\n
            if let Some(event_end) = buffer.find("\n\n") {
                // 提取完整的事件
                let event = buffer[..event_end].to_string();
                // 移除已处理的事件（包括\n\n）
                *buffer = buffer[event_end + 2..].to_string();

                debug!("Usage Collector - Found complete SSE event");

                // 解析SSE事件
                self.parse_and_process_sse_event(&event);
            } else {
                // 没有完整的事件，等待更多数据
                break;
            }
        }

        // 如果缓冲区太大（超过1MB），清空以防止内存泄漏
        if buffer.len() > 1024 * 1024 {
            debug!("Usage Collector - Buffer too large, clearing");
            buffer.clear();
        }
    }

    /// 解析并处理一个完整的SSE事件
    fn parse_and_process_sse_event(&self, event: &str) {
        let mut event_type: Option<String> = None;
        let mut data_lines: Vec<String> = Vec::new();

        // 解析SSE事件的字段
        for line in event.lines() {
            if let Some(stripped) = line.strip_prefix("event:").or_else(|| line.strip_prefix("event: ")) {
                event_type = Some(stripped.trim().to_string());
            } else if let Some(stripped) = line.strip_prefix("data:").or_else(|| line.strip_prefix("data: ")) {
                data_lines.push(stripped.to_string());
            }
        }

        // 如果没有data字段，跳过
        if data_lines.is_empty() {
            return;
        }

        // 合并所有data行（SSE规范允许多行data）
        let data = data_lines.join("\n");

        // 跳过 [DONE] 标记
        if data.trim() == "[DONE]" {
            debug!("Usage Collector - Skipping [DONE] marker");
            return;
        }

        debug!("Usage Collector - Event type: {:?}, Data length: {} bytes", event_type, data.len());

        // 尝试解析JSON
        match serde_json::from_str::<serde_json::Value>(&data) {
            Ok(json) => {
                debug!("Usage Collector - Successfully parsed JSON");
                self.extract_usage_from_json(&json);
            }
            Err(e) => {
                debug!("Usage Collector - Failed to parse JSON: {}", e);
            }
        }
    }

    /// 从JSON中提取usage信息
    fn extract_usage_from_json(&self, json: &serde_json::Value) {
        debug!("Usage Collector - Extracting usage from JSON, protocol: {:?}", self.route_config.protocol);

        match &self.route_config.protocol {
            TargetProtocol::Anthropic => {
                debug!("Usage Collector - Processing Anthropic protocol");

                // 检查JSON结构
                if let Some(event_type) = json.get("type").and_then(|v| v.as_str()) {
                    debug!("Usage Collector - Found event type: {}", event_type);

                    match event_type {
                        "message_start" => {
                            debug!("Usage Collector - Processing message_start event");

                            // message_start 包含 input_tokens
                            if let Some(message) = json.get("message") {
                                debug!("Usage Collector - Found message object: {}", serde_json::to_string(message).unwrap_or_else(|_| "Invalid".to_string()));

                                if let Some(usage) = message.get("usage") {
                                    debug!("Usage Collector - Found usage in message: {}", serde_json::to_string(usage).unwrap_or_else(|_| "Invalid".to_string()));

                                    if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_i64()) {
                                        *self.input_tokens.lock().unwrap() = Some(input as i32);
                                        debug!("Usage Collector - Collected input_tokens: {}", input);
                                    } else {
                                        debug!("Usage Collector - No input_tokens found in usage");
                                    }
                                } else {
                                    debug!("Usage Collector - No usage found in message");
                                }
                            } else {
                                debug!("Usage Collector - No message object found");
                            }
                        }
                        "message_delta" => {
                            debug!("Usage Collector - Processing message_delta event");

                            // message_delta 包含累积的 output_tokens
                            if let Some(usage) = json.get("usage") {
                                debug!("Usage Collector - Found usage in delta: {}", serde_json::to_string(usage).unwrap_or_else(|_| "Invalid".to_string()));

                                if let Some(output) = usage.get("output_tokens").and_then(|v| v.as_i64()) {
                                    *self.output_tokens.lock().unwrap() = Some(output as i32);
                                    debug!("Usage Collector - Updated output_tokens: {}", output);
                                } else {
                                    debug!("Usage Collector - No output_tokens found in usage");
                                }
                            } else {
                                debug!("Usage Collector - No usage found in message_delta");
                            }
                        }
                        "message_stop" => {
                            debug!("Usage Collector - Processing message_stop event, triggering usage report");
                            // 流结束，触发上报
                            self.report_usage();
                        }
                        _ => {
                            debug!("Usage Collector - Unhandled event type: {}", event_type);
                        }
                    }
                } else {
                    debug!("Usage Collector - No 'type' field found in JSON");
                    // 尝试查找其他可能的usage字段
                    if let Some(usage) = json.get("usage") {
                        debug!("Usage Collector - Found direct usage field: {}", serde_json::to_string(usage).unwrap_or_else(|_| "Invalid".to_string()));
                    }
                }
            }
            TargetProtocol::OpenAI | TargetProtocol::Custom(_) => {
                debug!("Usage Collector - Processing OpenAI/Custom protocol");

                // 首先检查是否是 Codex 的 response.completed 或 response.done 事件
                if let Some(event_type) = json.get("type").and_then(|v| v.as_str()) {
                    debug!("Usage Collector - Found event type: {}", event_type);

                    if event_type == "response.completed" || event_type == "response.done" {
                        debug!("Usage Collector - Processing Codex {} event", event_type);

                        // Codex 的 usage 信息在 response.usage 嵌套对象中
                        if let Some(response) = json.get("response") {
                            if let Some(usage) = response.get("usage") {
                                debug!("Usage Collector - Found Codex usage: {}", serde_json::to_string(usage).unwrap_or_else(|_| "Invalid".to_string()));

                                // Codex 使用 input_tokens 和 output_tokens (类似 Anthropic)
                                if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_i64()) {
                                    *self.input_tokens.lock().unwrap() = Some(input as i32);
                                    debug!("Usage Collector - Collected Codex input_tokens: {}", input);
                                }
                                if let Some(output) = usage.get("output_tokens").and_then(|v| v.as_i64()) {
                                    *self.output_tokens.lock().unwrap() = Some(output as i32);
                                    debug!("Usage Collector - Collected Codex output_tokens: {}", output);
                                }

                                // response.completed 表示流结束,触发上报
                                debug!("Usage Collector - Codex stream completed, triggering usage report");
                                self.report_usage();
                            } else {
                                debug!("Usage Collector - No usage found in Codex response object");
                            }
                        } else {
                            debug!("Usage Collector - No response object found in Codex event");
                        }
                        return; // 已处理,直接返回
                    }
                }

                // 标准 OpenAI 流式响应
                // 检查是否有usage字段（通常在最后一个chunk）
                if let Some(usage) = json.get("usage") {
                    debug!("Usage Collector - Found OpenAI usage: {}", serde_json::to_string(usage).unwrap_or_else(|_| "Invalid".to_string()));

                    // 支持两种字段名: prompt_tokens/completion_tokens (OpenAI) 和 input_tokens/output_tokens (兼容)
                    if let Some(input) = usage.get("prompt_tokens")
                        .or_else(|| usage.get("input_tokens"))
                        .and_then(|v| v.as_i64())
                    {
                        *self.input_tokens.lock().unwrap() = Some(input as i32);
                        debug!("Usage Collector - Collected input tokens: {}", input);
                    }
                    if let Some(output) = usage.get("completion_tokens")
                        .or_else(|| usage.get("output_tokens"))
                        .and_then(|v| v.as_i64())
                    {
                        *self.output_tokens.lock().unwrap() = Some(output as i32);
                        debug!("Usage Collector - Collected output tokens: {}", output);
                        // OpenAI在有usage时通常意味着流结束
                        self.report_usage();
                    }
                } else {
                    debug!("Usage Collector - No usage found in OpenAI response");
                }
            }
        }
    }

    /// 上报usage数据
    pub fn report_usage(&self) {
        let input = self.input_tokens.lock().unwrap().clone();
        let output = self.output_tokens.lock().unwrap().clone();

        debug!("Usage Collector - Attempting to report usage: input={:?}, output={:?}", input, output);

        if let (Some(input_tokens), Some(output_tokens)) = (input, output) {
            debug!("Usage Collector - Reporting usage: input={}, output={}", input_tokens, output_tokens);
            debug!("Usage Collector - Usage event details: request_id={}, model={}, api={}",
                   self.request_id, self.route_config.model, self.route_config.api_endpoint);

            self.telemetry.report_usage(UsageEvent {
                request_id: self.request_id.clone(),
                token: self.user_token.clone(),
                model: self.route_config.model.clone(),  // 请求的模型名
                api: self.route_config.api_endpoint.clone(),
                input_tokens,
                output_tokens,
                // 新增：使用RouteConfig中的ID字段
                model_id: self.route_config.model_id.clone(),
                provider_id: self.route_config.provider_id.clone(),
                provider_token_id: self.route_config.provider_token_id.clone(),
            });

            debug!("Usage Collector - Usage report sent successfully");
        } else {
            debug!("Usage Collector - Cannot report usage: missing tokens (input={:?}, output={:?})", input, output);
        }
    }

    /// 包装流，在每个chunk上收集usage信息
    pub async fn wrap_stream<S>(
        self: Arc<Self>,
        mut stream: S,
    ) -> impl Stream<Item = Result<Bytes>>
    where
        S: Stream<Item = Result<Bytes>> + Unpin,
    {
        async_stream::stream! {
            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        // 处理chunk提取usage信息
                        self.process_chunk(&chunk);
                        yield Ok(chunk);
                    }
                    Err(e) => {
                        yield Err(e);
                        break;
                    }
                }
            }

            // 流结束，确保上报usage（如果还没上报的话）
            self.report_usage();
        }
    }
}
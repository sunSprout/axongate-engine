use crate::error::{Error, Result};
use crate::models::{ClientProtocol, TargetProtocol};
use crate::protocol::{anthropic, openai, ProtocolAdapter};
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures::{Stream, StreamExt};
use serde_json::{json, Value};
use std::pin::Pin;
use tracing::{debug, error};

pub struct UniversalAdapter;

impl UniversalAdapter {
    pub fn new() -> Self {
        Self
    }

    // ================== SSE 解析辅助函数 ==================
    
    /// 解析 SSE 行，提取 event 和 data 字段
    /// SSE 格式示例:
    /// - OpenAI: "data: {...}\n\n"
    /// - Anthropic: "event: content_block_delta\ndata: {...}\n\n"
    fn parse_sse_line(line: &str) -> Option<(&str, &str)> {
        if let Some(pos) = line.find(':') {
            let (field, rest) = line.split_at(pos);
            let value = rest.trim_start_matches(':').trim();
            Some((field.trim(), value))
        } else {
            None
        }
    }
    
    /// 生成 SSE 格式的字符串
    fn format_sse(event: Option<&str>, data: &str) -> String {
        if let Some(event) = event {
            format!("event: {}\ndata: {}\n\n", event, data)
        } else {
            format!("data: {}\n\n", data)
        }
    }

    // ================== OpenAI -> Anthropic 流转换 ==================
    
    /// 将 OpenAI 流式响应转换为 Anthropic 格式
    /// 
    /// OpenAI 格式示例:
    /// ```
    /// data: {"id":"chatcmpl-123","choices":[{"delta":{"role":"assistant","content":""},"finish_reason":null}]}
    /// data: {"id":"chatcmpl-123","choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}
    /// data: {"id":"chatcmpl-123","choices":[{"delta":{"content":""},"finish_reason":"stop"}]}
    /// data: {"id":"chatcmpl-123","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}
    /// data: [DONE]
    /// ```
    /// 
    /// Anthropic 格式示例:
    /// ```
    /// event: message_start
    /// data: {"type":"message_start","message":{...}}
    /// 
    /// event: content_block_delta
    /// data: {"type":"content_block_delta","delta":{"text":"Hello"}}
    /// 
    /// event: message_delta
    /// data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}
    /// 
    /// event: message_stop
    /// data: {"type":"message_stop"}
    /// ```
    fn convert_openai_to_anthropic_stream(
        &self,
        stream: impl Stream<Item = Result<Bytes>> + Send + 'static,
    ) -> impl Stream<Item = Result<Bytes>> + Send + 'static {
        let mut buffer = BytesMut::new();
        let mut message_started = false;
        let mut content_block_started = false;
        let mut message_id = String::new();
        let mut model = String::new();
        let mut usage_tokens = None;
        
        stream.then(move |chunk_result| {
            let mut buffer = buffer.clone();
            let mut message_started = message_started;
            let mut content_block_started = content_block_started;
            let mut message_id = message_id.clone();
            let mut model = model.clone();
            let mut usage_tokens = usage_tokens.clone();
            
            async move {
                match chunk_result {
                    Ok(chunk) => {
                        // 将新数据追加到缓冲区
                        buffer.extend_from_slice(&chunk);
                        
                        let mut output = Vec::new();
                        
                        // 按行处理缓冲区
                        while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
                            let line = buffer.split_to(pos + 1);
                            let line_str = String::from_utf8_lossy(&line);
                            let line_str = line_str.trim();
                            
                            if line_str.is_empty() {
                                continue;
                            }
                            
                            // 解析 OpenAI SSE 格式: "data: ..."
                            if let Some((field, value)) = Self::parse_sse_line(&line_str) {
                                if field == "data" {
                                    // 处理结束标记
                                    if value == "[DONE]" {
                                        // 生成 Anthropic 结束事件
                                        if content_block_started {
                                            let stop_event = json!({
                                                "type": "content_block_stop",
                                                "index": 0
                                            });
                                            output.push(Self::format_sse(
                                                Some("content_block_stop"),
                                                &stop_event.to_string()
                                            ));
                                        }
                                        
                                        // 生成 message_delta，包含 usage 信息（如果有）
                                        let delta_event = if let Some(usage) = usage_tokens {
                                            json!({
                                                "type": "message_delta",
                                                "delta": {"stop_reason": "end_turn"},
                                                "usage": {"output_tokens": usage}
                                            })
                                        } else {
                                            json!({
                                                "type": "message_delta",
                                                "delta": {"stop_reason": "end_turn"}
                                            })
                                        };
                                        output.push(Self::format_sse(
                                            Some("message_delta"),
                                            &delta_event.to_string()
                                        ));
                                        
                                        let stop_event = json!({"type": "message_stop"});
                                        output.push(Self::format_sse(
                                            Some("message_stop"),
                                            &stop_event.to_string()
                                        ));
                                        
                                        break;
                                    }
                                    
                                    // 解析 OpenAI JSON 数据
                                    if let Ok(json_data) = serde_json::from_str::<Value>(value) {
                                        // 检查是否有 usage 信息（某些实现会单独发送 usage chunk）
                                        if let Some(usage) = json_data.get("usage") {
                                            if !usage.is_null() {
                                                usage_tokens = usage.get("completion_tokens")
                                                    .and_then(|t| t.as_i64())
                                                    .map(|t| t as i32);
                                            }
                                        }
                                        
                                        // 提取元数据
                                        if !message_started {
                                            message_id = json_data["id"].as_str().unwrap_or("msg_unknown").to_string();
                                            model = json_data["model"].as_str().unwrap_or("unknown").to_string();
                                            
                                            // 生成 message_start 事件
                                            let start_event = json!({
                                                "type": "message_start",
                                                "message": {
                                                    "id": message_id,
                                                    "type": "message",
                                                    "role": "assistant",
                                                    "content": [],
                                                    "model": model,
                                                    "stop_reason": null,
                                                    "stop_sequence": null
                                                }
                                            });
                                            output.push(Self::format_sse(
                                                Some("message_start"),
                                                &start_event.to_string()
                                            ));
                                            message_started = true;
                                        }
                                        
                                        // 处理内容增量
                                        if let Some(choices) = json_data["choices"].as_array() {
                                            if let Some(choice) = choices.get(0) {
                                                if let Some(delta) = choice.get("delta") {
                                                    // 检查是否有角色信息（第一个 chunk）
                                                    if delta.get("role").is_some() && !content_block_started {
                                                        let block_start = json!({
                                                            "type": "content_block_start",
                                                            "index": 0,
                                                            "content_block": {
                                                                "type": "text",
                                                                "text": ""
                                                            }
                                                        });
                                                        output.push(Self::format_sse(
                                                            Some("content_block_start"),
                                                            &block_start.to_string()
                                                        ));
                                                        content_block_started = true;
                                                    }
                                                    
                                                    // 处理内容（跳过空内容）
                                                    if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                                                        if !content.is_empty() {
                                                            if !content_block_started {
                                                                let block_start = json!({
                                                                    "type": "content_block_start",
                                                                    "index": 0,
                                                                    "content_block": {
                                                                        "type": "text",
                                                                        "text": ""
                                                                    }
                                                                });
                                                                output.push(Self::format_sse(
                                                                    Some("content_block_start"),
                                                                    &block_start.to_string()
                                                                ));
                                                                content_block_started = true;
                                                            }
                                                            
                                                            let delta_event = json!({
                                                                "type": "content_block_delta",
                                                                "index": 0,
                                                                "delta": {
                                                                    "type": "text_delta",
                                                                    "text": content
                                                                }
                                                            });
                                                            output.push(Self::format_sse(
                                                                Some("content_block_delta"),
                                                                &delta_event.to_string()
                                                            ));
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        
                        // 返回转换后的数据
                        if !output.is_empty() {
                            Ok(Bytes::from(output.join("")))
                        } else {
                            Ok(Bytes::new()) // 返回空数据，等待更多输入
                        }
                    }
                    Err(e) => Err(e),
                }
            }
        })
    }

    // ================== Anthropic -> OpenAI 流转换 ==================
    
    /// 将 Anthropic 流式响应转换为 OpenAI 格式
    /// 
    /// Anthropic 输入格式:
    /// ```
    /// event: message_start
    /// data: {"type":"message_start","message":{...}}
    /// 
    /// event: content_block_delta  
    /// data: {"type":"content_block_delta","delta":{"text":"Hello"}}
    /// 
    /// event: message_delta
    /// data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}
    /// 
    /// event: message_stop
    /// data: {"type":"message_stop"}
    /// ```
    /// 
    /// OpenAI 输出格式:
    /// ```
    /// data: {"id":"chatcmpl-123","choices":[{"delta":{"role":"assistant","content":""},"finish_reason":null}]}
    /// data: {"id":"chatcmpl-123","choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}
    /// data: {"id":"chatcmpl-123","choices":[{"delta":{"content":""},"finish_reason":"stop"}]}
    /// data: {"id":"chatcmpl-123","choices":[],"usage":{"prompt_tokens":0,"completion_tokens":5,"total_tokens":5}}
    /// data: [DONE]
    /// ```
    fn convert_anthropic_to_openai_stream(
        &self,
        stream: impl Stream<Item = Result<Bytes>> + Send + 'static,
    ) -> impl Stream<Item = Result<Bytes>> + Send + 'static {
        let mut buffer = BytesMut::new();
        let mut current_event: Option<String> = None;
        let mut message_id = String::from("chatcmpl-unknown");
        let mut model = String::from("unknown");
        let mut usage_info: Option<Value> = None;
        
        stream.then(move |chunk_result| {
            let mut buffer = buffer.clone();
            let mut current_event = current_event.clone();
            let mut message_id = message_id.clone();
            let mut model = model.clone();
            let mut usage_info = usage_info.clone();
            
            async move {
                match chunk_result {
                    Ok(chunk) => {
                        // 将新数据追加到缓冲区
                        buffer.extend_from_slice(&chunk);
                        
                        let mut output = Vec::new();
                        
                        // 按行处理缓冲区
                        while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
                            let line = buffer.split_to(pos + 1);
                            let line_str = String::from_utf8_lossy(&line);
                            let line_str = line_str.trim();
                            
                            if line_str.is_empty() {
                                // 空行表示事件结束
                                current_event = None;
                                continue;
                            }
                            
                            // 解析 Anthropic SSE 格式
                            if let Some((field, value)) = Self::parse_sse_line(&line_str) {
                                match field {
                                    "event" => {
                                        current_event = Some(value.to_string());
                                    }
                                    "data" => {
                                        if let Ok(json_data) = serde_json::from_str::<Value>(value) {
                                            match current_event.as_deref() {
                                                Some("message_start") => {
                                                    // 提取消息元数据
                                                    if let Some(message) = json_data.get("message") {
                                                        message_id = message["id"]
                                                            .as_str()
                                                            .unwrap_or("chatcmpl-unknown")
                                                            .to_string();
                                                        model = message["model"]
                                                            .as_str()
                                                            .unwrap_or("unknown")
                                                            .to_string();
                                                    }
                                                    
                                                    // 生成第一个 OpenAI chunk（包含角色）
                                                    let openai_chunk = json!({
                                                        "id": message_id,
                                                        "object": "chat.completion.chunk",
                                                        "created": chrono::Utc::now().timestamp(),
                                                        "model": model,
                                                        "choices": [{
                                                            "index": 0,
                                                            "delta": {"role": "assistant", "content": ""},
                                                            "finish_reason": null
                                                        }],
                                                        "usage": null
                                                    });
                                                    output.push(Self::format_sse(None, &openai_chunk.to_string()));
                                                }
                                                Some("content_block_delta") => {
                                                    // 转换内容增量
                                                    if let Some(delta) = json_data.get("delta") {
                                                        if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                                            let openai_chunk = json!({
                                                                "id": message_id,
                                                                "object": "chat.completion.chunk",
                                                                "created": chrono::Utc::now().timestamp(),
                                                                "model": model,
                                                                "choices": [{
                                                                    "index": 0,
                                                                    "delta": {"content": text},
                                                                    "finish_reason": null
                                                                }],
                                                                "usage": null
                                                            });
                                                            output.push(Self::format_sse(None, &openai_chunk.to_string()));
                                                        }
                                                    }
                                                }
                                                Some("message_delta") => {
                                                    // 提取 usage 信息和结束原因
                                                    let stop_reason = json_data["delta"]["stop_reason"]
                                                        .as_str()
                                                        .unwrap_or("stop");
                                                    
                                                    // 保存 usage 信息
                                                    if let Some(usage) = json_data.get("usage") {
                                                        let output_tokens = usage["output_tokens"].as_i64().unwrap_or(0);
                                                        // Anthropic 不提供 prompt_tokens，设为 0
                                                        usage_info = Some(json!({
                                                            "prompt_tokens": 0,
                                                            "completion_tokens": output_tokens,
                                                            "total_tokens": output_tokens
                                                        }));
                                                    }
                                                    
                                                    // 生成带 finish_reason 的 chunk
                                                    let openai_chunk = json!({
                                                        "id": message_id,
                                                        "object": "chat.completion.chunk",
                                                        "created": chrono::Utc::now().timestamp(),
                                                        "model": model,
                                                        "choices": [{
                                                            "index": 0,
                                                            "delta": {"content": ""},
                                                            "finish_reason": stop_reason
                                                        }],
                                                        "usage": null
                                                    });
                                                    output.push(Self::format_sse(None, &openai_chunk.to_string()));
                                                }
                                                Some("message_stop") => {
                                                    // 如果有 usage 信息，生成单独的 usage chunk（像阿里云的格式）
                                                    if let Some(ref usage) = usage_info {
                                                        let usage_chunk = json!({
                                                            "id": message_id,
                                                            "object": "chat.completion.chunk",
                                                            "created": chrono::Utc::now().timestamp(),
                                                            "model": model,
                                                            "choices": [],
                                                            "usage": usage
                                                        });
                                                        output.push(Self::format_sse(None, &usage_chunk.to_string()));
                                                    }
                                                    
                                                    // 生成 [DONE] 标记
                                                    output.push(Self::format_sse(None, "[DONE]"));
                                                }
                                                _ => {
                                                    // 忽略其他事件类型（如 content_block_start, content_block_stop）
                                                    debug!("Ignoring Anthropic event type: {:?}", current_event);
                                                }
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        
                        // 返回转换后的数据
                        if !output.is_empty() {
                            Ok(Bytes::from(output.join("")))
                        } else {
                            Ok(Bytes::new()) // 返回空数据，等待更多输入
                        }
                    }
                    Err(e) => Err(e),
                }
            }
        })
    }

    // ================== 原有的请求/响应转换函数 ==================

    fn openai_to_anthropic(
        openai_req: &openai::OpenAIRequest,
        target_model: &str,
    ) -> Result<anthropic::AnthropicRequest> {
        let mut messages = Vec::new();
        let mut system_prompt = None;

        for msg in &openai_req.messages {
            match msg.role.as_str() {
                "system" => {
                    if let openai::MessageContent::Text(text) = &msg.content {
                        system_prompt = Some(text.clone());
                    }
                }
                "user" | "assistant" => {
                    let content = match &msg.content {
                        openai::MessageContent::Text(text) => {
                            anthropic::MessageContent::Text(text.clone())
                        }
                        _ => anthropic::MessageContent::Text("".to_string()),
                    };

                    messages.push(anthropic::Message {
                        role: msg.role.clone(),
                        content,
                    });
                }
                _ => {}
            }
        }

        Ok(anthropic::AnthropicRequest {
            model: target_model.to_string(),
            messages,
            max_tokens: openai_req.max_tokens.unwrap_or(1024),
            temperature: openai_req.temperature,
            top_p: openai_req.top_p,
            top_k: None,
            stream: openai_req.stream,
            system: system_prompt,
            extra: Value::Object(Default::default()),
        })
    }

    fn anthropic_to_openai(
        anthropic_req: &anthropic::AnthropicRequest,
        target_model: &str,
    ) -> Result<openai::OpenAIRequest> {
        let mut messages = Vec::new();

        if let Some(system) = &anthropic_req.system {
            messages.push(openai::Message {
                role: "system".to_string(),
                content: openai::MessageContent::Text(system.clone()),
            });
        }

        for msg in &anthropic_req.messages {
            let content = match &msg.content {
                anthropic::MessageContent::Text(text) => openai::MessageContent::Text(text.clone()),
                _ => openai::MessageContent::Text("".to_string()),
            };

            messages.push(openai::Message {
                role: msg.role.clone(),
                content,
            });
        }

        Ok(openai::OpenAIRequest {
            model: target_model.to_string(),
            messages,
            max_tokens: Some(anthropic_req.max_tokens),
            temperature: anthropic_req.temperature,
            top_p: anthropic_req.top_p,
            stream: anthropic_req.stream,
            frequency_penalty: None,
            presence_penalty: None,
            extra: Value::Object(Default::default()),
        })
    }

    fn openai_response_to_anthropic(
        openai_resp: &openai::OpenAIResponse,
    ) -> Result<anthropic::AnthropicResponse> {
        let first_choice = openai_resp
            .choices
            .first()
            .ok_or_else(|| Error::Protocol("No choices in OpenAI response".into()))?;

        let text = match &first_choice.message.content {
            openai::MessageContent::Text(text) => text.clone(),
            _ => "".to_string(),
        };

        Ok(anthropic::AnthropicResponse {
            id: openai_resp.id.clone(),
            response_type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![anthropic::ContentBlock::Text { text }],
            model: openai_resp.model.clone(),
            stop_reason: first_choice.finish_reason.clone(),
            stop_sequence: None,
            usage: anthropic::Usage {
                input_tokens: openai_resp.usage.prompt_tokens,
                output_tokens: openai_resp.usage.completion_tokens,
            },
        })
    }

    fn anthropic_response_to_openai(
        anthropic_resp: &anthropic::AnthropicResponse,
    ) -> Result<openai::OpenAIResponse> {
        let text = anthropic_resp
            .content
            .iter()
            .filter_map(|block| match block {
                anthropic::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        Ok(openai::OpenAIResponse {
            id: anthropic_resp.id.clone(),
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp(),
            model: anthropic_resp.model.clone(),
            choices: vec![openai::Choice {
                index: 0,
                message: openai::Message {
                    role: "assistant".to_string(),
                    content: openai::MessageContent::Text(text),
                },
                finish_reason: anthropic_resp.stop_reason.clone(),
            }],
            usage: openai::Usage {
                prompt_tokens: anthropic_resp.usage.input_tokens,
                completion_tokens: anthropic_resp.usage.output_tokens,
                total_tokens: anthropic_resp.usage.input_tokens
                    + anthropic_resp.usage.output_tokens,
            },
        })
    }
}

#[async_trait]
impl ProtocolAdapter for UniversalAdapter {
    async fn transform_request(
        &self,
        source_protocol: &ClientProtocol,
        target_protocol: &TargetProtocol,
        target_model: &str,
        request_body: Bytes,
    ) -> Result<Bytes> {
        let json_value: Value = serde_json::from_slice(&request_body)?;

        let transformed = match (source_protocol, target_protocol) {
            // OpenAI 同类型替换 换模型就好
            (ClientProtocol::OpenAI, TargetProtocol::OpenAI) => {
                // 对于OpenAI到OpenAI，需要替换模型名
                let mut json = json_value;
                // 这里是map 所以insert是新建或替换 没有重复
                if let Value::Object(ref mut obj) = json {
                    obj.insert("model".to_string(), Value::String(target_model.to_string()));
                }
                json
            }
            (ClientProtocol::OpenAI, TargetProtocol::Anthropic) => {
                let openai_req: openai::OpenAIRequest = serde_json::from_value(json_value)?;
                let anthropic_req = Self::openai_to_anthropic(&openai_req, target_model)?;
                serde_json::to_value(anthropic_req)?
            }
            (ClientProtocol::Anthropic, TargetProtocol::OpenAI) => {
                let anthropic_req: anthropic::AnthropicRequest =
                    serde_json::from_value(json_value)?;
                let openai_req = Self::anthropic_to_openai(&anthropic_req, target_model)?;
                serde_json::to_value(openai_req)?
            }
            // Anthropic 同类型替换 换模型就好
            (ClientProtocol::Anthropic, TargetProtocol::Anthropic) => {
                // 对于Anthropic到Anthropic，需要替换模型名
                let mut json = json_value;
                if let Value::Object(ref mut obj) = json {
                    obj.insert("model".to_string(), Value::String(target_model.to_string()));
                }
                json
            }
            _ => {
                return Err(Error::Protocol(format!(
                    "Unsupported protocol conversion: {:?} -> {:?}",
                    source_protocol, target_protocol
                )));
            }
        };

        Ok(Bytes::from(serde_json::to_vec(&transformed)?))
    }

    async fn transform_response(
        &self,
        source_protocol: &TargetProtocol,
        target_protocol: &ClientProtocol,
        response_body: Bytes,
    ) -> Result<Bytes> {
        let json_value: Value = serde_json::from_slice(&response_body)?;

        let transformed = match (source_protocol, target_protocol) {
            (TargetProtocol::OpenAI, ClientProtocol::OpenAI) => json_value,
            (TargetProtocol::OpenAI, ClientProtocol::Anthropic) => {
                let openai_resp: openai::OpenAIResponse = serde_json::from_value(json_value)?;
                let anthropic_resp = Self::openai_response_to_anthropic(&openai_resp)?;
                serde_json::to_value(anthropic_resp)?
            }
            (TargetProtocol::Anthropic, ClientProtocol::OpenAI) => {
                let anthropic_resp: anthropic::AnthropicResponse =
                    serde_json::from_value(json_value)?;
                let openai_resp = Self::anthropic_response_to_openai(&anthropic_resp)?;
                serde_json::to_value(openai_resp)?
            }
            (TargetProtocol::Anthropic, ClientProtocol::Anthropic) => json_value,
            _ => {
                return Err(Error::Protocol(format!(
                    "Unsupported protocol conversion: {:?} -> {:?}",
                    source_protocol, target_protocol
                )));
            }
        };

        Ok(Bytes::from(serde_json::to_vec(&transformed)?))
    }

    async fn transform_stream_chunk(
        &self,
        source_protocol: &TargetProtocol,
        target_protocol: &ClientProtocol,
        stream: impl Stream<Item = Result<Bytes>> + Send + 'static,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>> {
        match (source_protocol, target_protocol) {
            // OpenAI -> OpenAI: 直接透传
            (TargetProtocol::OpenAI, ClientProtocol::OpenAI) => {
                debug!("OpenAI -> OpenAI streaming: passthrough");
                Ok(Box::pin(stream))
            }
            
            // Anthropic -> Anthropic: 直接透传
            (TargetProtocol::Anthropic, ClientProtocol::Anthropic) => {
                debug!("Anthropic -> Anthropic streaming: passthrough");
                Ok(Box::pin(stream))
            }
            
            // OpenAI -> Anthropic: 转换 OpenAI SSE 格式到 Anthropic SSE 格式
            (TargetProtocol::OpenAI, ClientProtocol::Anthropic) => {
                debug!("OpenAI -> Anthropic streaming conversion");
                let converted_stream = self.convert_openai_to_anthropic_stream(stream);
                Ok(Box::pin(converted_stream))
            }
            
            // Anthropic -> OpenAI: 转换 Anthropic SSE 格式到 OpenAI SSE 格式
            (TargetProtocol::Anthropic, ClientProtocol::OpenAI) => {
                debug!("Anthropic -> OpenAI streaming conversion");
                let converted_stream = self.convert_anthropic_to_openai_stream(stream);
                Ok(Box::pin(converted_stream))
            }
            
            _ => {
                error!("Unsupported streaming protocol conversion: {:?} -> {:?}", source_protocol, target_protocol);
                Ok(Box::pin(stream)) // 降级为透传
            }
        }
    }
}

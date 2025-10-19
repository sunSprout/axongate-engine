use serde::{Deserialize, Serialize};

/// 客户端协议类型
/// 定义客户端请求使用的协议格式
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientProtocol {
    /// OpenAI协议格式（如ChatGPT API）
    OpenAI,
    /// Anthropic协议格式（如Claude API）
    Anthropic,
    /// 自定义协议格式，包含协议名称
    Custom(String),
}

/// 目标服务协议类型
/// 定义转发到上游LLM服务时使用的协议格式
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetProtocol {
    /// OpenAI协议格式
    OpenAI,
    /// Anthropic协议格式
    Anthropic,
    /// 自定义协议格式，包含协议名称
    Custom(String),
}

/// 路由配置信息
/// 包含将请求路由到目标服务所需的完整配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteConfig {
    /// 供应商的API令牌/密钥
    pub token: String,
    /// 目标模型名称（如"gpt-4", "claude-3"）
    pub model: String,
    /// 目标API端点URL
    #[serde(rename = "api")]
    pub api_endpoint: String,
    /// 目标服务使用的协议类型
    pub protocol: TargetProtocol,

    // 新增ID字段
    /// 模型ID
    #[serde(rename = "model_id")]
    pub model_id: String,
    /// 供应商ID
    #[serde(rename = "provider_id")]
    pub provider_id: String,
    /// 供应商Token ID
    #[serde(rename = "provider_token_id")]
    pub provider_token_id: String,
}

/// 路由解析请求
/// 向业务后端请求路由信息时的请求结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRequest {
    /// 用户令牌，用于认证和查找路由配置
    pub token: String,
    /// 请求的模型名称
    pub model: String,
}

/// 路由解析响应
/// 业务后端返回的路由信息响应结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteResponse {
    /// 响应状态码（0表示成功）
    pub code: i32,
    /// 请求是否成功
    pub success: bool,
    /// 响应消息（错误时包含错误信息）
    pub message: String,
    /// 路由配置列表（可能包含多个备选路由）
    pub data: Vec<RouteConfig>,
}

/// 错误事件
/// 用于记录和上报代理请求的错误信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEvent {
    /// 用户令牌
    pub token: String,
    /// 使用的模型名称
    pub model: String,
    /// 调用的API端点
    pub api: String,
    /// 错误描述
    pub msg: String,

    // 新增ID字段 - 精确标识错误来源
    /// 供应商Token ID
    #[serde(rename = "provider_token_id", skip_serializing_if = "Option::is_none")]
    pub provider_token_id: Option<String>,
}

/// Usage事件
/// 用于记录和上报Token使用情况
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    /// 请求ID（用于去重）
    pub request_id: String,
    /// 用户令牌
    pub token: String,
    /// 使用的模型名称
    pub model: String,
    /// 调用的API端点
    pub api: String,
    /// 输入Token数
    pub input_tokens: i32,
    /// 输出Token数
    pub output_tokens: i32,

    // 新增ID字段 - 精确计费
    /// 模型ID
    #[serde(rename = "model_id")]
    pub model_id: String,
    /// 供应商ID
    #[serde(rename = "provider_id")]
    pub provider_id: String,
    /// 供应商Token ID
    #[serde(rename = "provider_token_id")]
    pub provider_token_id: String,
}

/// 遥测响应
/// 业务后端接收遥测事件后的响应结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryResponse {
    /// 响应状态码（0表示成功）
    pub code: i32,
    /// 上报是否成功
    pub success: bool,
    /// 响应消息
    pub message: String,
    /// 可选的响应数据（JSON格式）
    pub data: Option<serde_json::Value>,
}
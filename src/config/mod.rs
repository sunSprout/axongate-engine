use serde::{Deserialize, Serialize};
use std::time::Duration;
use crate::error::Result;

/// AI网关引擎的主配置结构
/// 包含服务器、业务API、缓存和代理等各个模块的配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// 服务器相关配置
    pub server: ServerConfig,
    /// 后端业务API配置
    pub business_api: BusinessApiConfig,
    /// 缓存配置
    pub cache: CacheConfig,
    /// 代理转发配置
    pub proxy: ProxyConfig,
}

/// 服务器配置
/// 定义HTTP服务器的监听地址、端口和工作线程数
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    /// 服务器监听地址，例如 "0.0.0.0" 或 "127.0.0.1"
    pub host: String,
    /// 服务器监听端口，默认为8080
    pub port: u16,
    /// 工作线程数，用于处理并发请求
    pub workers: usize,
}

/// 业务API配置
/// 用于与后端业务服务通信的配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BusinessApiConfig {
    /// 后端业务API的基础URL，例如 "http://localhost:3000"
    pub base_url: String,
    /// API请求超时时间，使用humantime格式（如 "5s", "30s"）
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    /// 失败重试次数
    pub retry_attempts: u32,
}

/// 缓存配置
/// 用于配置路由信息和其他数据的缓存策略
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CacheConfig {
    /// 缓存类型，支持内存缓存或Redis
    #[serde(rename = "type")]
    pub cache_type: CacheType,
    /// 缓存过期时间（TTL），使用humantime格式
    /// 滑动TTL：每次命中时刷新
    #[serde(with = "humantime_serde")]
    pub ttl: Duration,
    /// 缓存最大生存时间（硬过期），使用humantime格式
    /// 无论访问频率，到达此时间后强制失效
    #[serde(with = "humantime_serde", default = "default_max_lifetime")]
    pub max_lifetime: Duration,
    /// 缓存最大条目数
    pub max_size: usize,
}

/// 默认的最大生存时间：24小时
fn default_max_lifetime() -> Duration {
    Duration::from_secs(24 * 3600)
}

/// 缓存类型枚举
/// 定义支持的缓存后端类型
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheType {
    /// 内存缓存，适用于单实例部署
    Memory,
    /// Redis缓存，适用于分布式部署
    Redis,
}

/// 代理配置
/// 用于配置HTTP代理转发的相关参数
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProxyConfig {
    /// 代理请求超时时间，针对上游LLM服务的请求
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    /// 最大并发连接数
    pub max_connections: usize,
    /// 是否启用HTTP Keep-Alive
    pub keep_alive: bool,
    /// 请求失败重试次数
    pub retry_attempts: u32,
}

impl Config {
    /// 从配置文件加载配置
    /// 
    /// # 参数
    /// * `path` - 配置文件路径（支持YAML、TOML、JSON等格式）
    /// 
    /// # 返回
    /// * `Result<Self>` - 成功返回Config实例，失败返回错误
    /// 
    /// # 说明
    /// 1. 首先从指定文件加载配置
    /// 2. 然后从环境变量覆盖配置（前缀为GATEWAY，分隔符为__）
    ///    例如：GATEWAY__SERVER__PORT=8081 会覆盖 server.port
    pub fn from_file(path: &str) -> Result<Self> {
        let settings = config::Config::builder()
            .add_source(config::File::with_name(path))
            .add_source(config::Environment::with_prefix("GATEWAY").separator("__"))
            .build()
            .map_err(|e| crate::error::Error::Config(e.to_string()))?;
        
        settings
            .try_deserialize()
            .map_err(|e| crate::error::Error::Config(e.to_string()))
    }
    
    /// 创建默认配置
    /// 
    /// # 默认值
    /// - 服务器：监听 0.0.0.0:8080，4个工作线程
    /// - 业务API：连接 http://localhost:3000，超时5秒，重试3次
    /// - 缓存：内存缓存，TTL 5分钟，最大1万条
    /// - 代理：超时30秒，最大500连接，启用Keep-Alive，重试3次
    pub fn default() -> Self {
        Self {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 8080,
                workers: 4,
            },
            business_api: BusinessApiConfig {
                base_url: "http://localhost:3000".to_string(),
                timeout: Duration::from_secs(5),
                retry_attempts: 3,
            },
            cache: CacheConfig {
                cache_type: CacheType::Memory,
                ttl: Duration::from_secs(300),
                max_lifetime: Duration::from_secs(24 * 3600),
                max_size: 10000,
            },
            proxy: ProxyConfig {
                timeout: Duration::from_secs(30),
                max_connections: 500,
                keep_alive: true,
                retry_attempts: 3,
            },
        }
    }
}
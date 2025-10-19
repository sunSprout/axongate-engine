use crate::models::RouteConfig;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 缓存条目结构
///
/// 存储特定用户token和模型组合的路由配置列表及过期时间
#[derive(Clone)]
struct CacheEntry {
    /// 可用的路由配置列表
    /// 包含多个供应商的API端点，支持故障转移
    configs: Vec<RouteConfig>,

    /// 创建时间点
    /// 用于计算硬过期时间
    created_at: Instant,

    /// 软过期时间点（滑动TTL）
    /// 每次命中时会刷新，但不超过硬过期时间
    expires_at: Instant,

    /// 硬过期时间点（最大生存时间）
    /// 无论访问频率如何，到达此时间后强制失效
    hard_expires_at: Instant,
}

/// 路由缓存管理器
///
/// 使用DashMap实现线程安全的并发缓存，
/// 支持自动过期清理和故障节点移除
///
/// 缓存策略：
/// - 滑动TTL：高频访问时自动续期
/// - 硬过期：最大生存时间到达后强制失效
#[derive(Clone)]
pub struct Cache {
    /// Key格式: "user_token:model_name"
    /// Value: 缓存条目(路由配置列表+过期时间)
    storage: Arc<DashMap<String, CacheEntry>>,

    /// 缓存生存时间(TTL) - 滑动过期
    /// 每次命中时会刷新软过期时间
    ttl: Duration,

    /// 缓存最大生存时间 - 硬过期
    /// 无论访问频率，到达此时间后强制失效
    max_lifetime: Duration,
}

impl Cache {
    /// 创建新的缓存实例
    ///
    /// # 参数
    /// * `ttl` - 缓存条目的滑动生存时间（每次命中时刷新）
    /// * `max_lifetime` - 缓存条目的最大生存时间（硬过期）
    ///
    /// # 返回
    /// 返回初始化完成的缓存实例
    pub fn new(ttl: Duration, max_lifetime: Duration) -> Self {
        Self {
            storage: Arc::new(DashMap::new()),
            ttl,
            max_lifetime,
        }
    }

    /// 生成缓存键
    ///
    /// 将用户token和模型名组合成唯一的缓存键
    /// 格式: "token:model"
    fn make_key(token: &str, model: &str) -> String {
        format!("{}:{}", token, model)
    }

    /// 获取缓存的路由配置
    ///
    /// # 参数
    /// * `token` - 用户认证token
    /// * `model` - AI模型名称
    ///
    /// # 返回
    /// * `Some(Vec<RouteConfig>)` - 有效的缓存配置列表
    /// * `None` - 缓存未命中或已过期
    ///
    /// # 行为
    /// - 检查硬过期和软过期，任一过期则删除条目
    /// - 如果未过期，自动刷新软过期时间（滑动续期）
    /// - 返回的是配置列表的克隆，避免并发修改问题
    pub async fn get(&self, token: &str, model: &str) -> Option<Vec<RouteConfig>> {
        let key = Self::make_key(token, model);
        let now = Instant::now();
        let mut need_remove = false;

        // 第一阶段：检查过期（只读锁）
        if let Some(entry) = self.storage.get(&key) {
            // 硬过期检查：到达最大生存时间
            if now >= entry.hard_expires_at {
                need_remove = true;
            }
            // 软过期检查：到达滑动TTL过期时间
            else if now >= entry.expires_at {
                need_remove = true;
            }
        }

        // 第二阶段：删除过期条目
        if need_remove {
            self.storage.remove(&key);
            return None;
        }

        // 第三阶段：刷新软过期时间并返回（写锁）
        if let Some(mut entry) = self.storage.get_mut(&key) {
            // 滑动续期：刷新软过期时间，但不超过硬过期时间
            let new_expires_at = (now + self.ttl).min(entry.hard_expires_at);
            entry.expires_at = new_expires_at;

            let configs = entry.configs.clone();
            // 显式释放写锁
            drop(entry);

            return Some(configs);
        }

        None
    }

    /// 设置缓存的路由配置
    ///
    /// # 参数
    /// * `token` - 用户认证token
    /// * `model` - AI模型名称
    /// * `configs` - 要缓存的路由配置列表
    ///
    /// # 行为
    /// - 如果键已存在，会覆盖原有值
    /// - 自动设置创建时间、软过期时间和硬过期时间
    /// - 软过期时间 = min(now + ttl, now + max_lifetime)
    /// - 硬过期时间 = now + max_lifetime
    pub async fn set(&self, token: &str, model: &str, configs: Vec<RouteConfig>) {
        let key = Self::make_key(token, model);
        let now = Instant::now();

        let entry = CacheEntry {
            configs,
            created_at: now,
            hard_expires_at: now + self.max_lifetime,
            expires_at: (now + self.ttl).min(now + self.max_lifetime),
        };

        self.storage.insert(key, entry);
    }

    /// 从缓存中移除失败的路由配置
    ///
    /// 当某个路由配置请求失败时，将其从缓存中移除，
    /// 避免后续请求继续使用失败的端点
    ///
    /// # 参数
    /// * `token` - 用户认证token
    /// * `model` - AI模型名称
    /// * `failed_config` - 需要移除的失败配置
    ///
    /// # 行为
    /// - 只移除匹配的特定配置(token和api_endpoint都相同)
    /// - 如果移除后配置列表为空，则删除整个缓存条目
    pub async fn remove_config(&self, token: &str, model: &str, failed_config: &RouteConfig) {
        let key = Self::make_key(token, model);

        let mut should_remove_entry = false;

        if let Some(mut entry) = self.storage.get_mut(&key) {
            // 保留不匹配失败配置的其他配置
            entry.configs.retain(|c| {
                c.token != failed_config.token || c.api_endpoint != failed_config.api_endpoint
            });

            // DashMap 的 RefMut 在作用域结束前会持有写锁。
            // 记录需要删除的状态，先释放锁再执行 remove，避免死锁。
            if entry.configs.is_empty() {
                should_remove_entry = true;
            }
        }

        if should_remove_entry {
            self.storage.remove(&key);
        }
    }

    /// 清空所有缓存
    ///
    /// 用于强制刷新缓存或系统重置
    pub async fn clear(&self) {
        self.storage.clear();
    }
}

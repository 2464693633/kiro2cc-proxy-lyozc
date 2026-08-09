// Copyright (c) 2026 Harllan He. Licensed under MIT.
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum TlsBackend {
    #[default]
    Rustls,
    NativeTls,
}

/// 自动拉取 API Key 配置
///
/// 从 `url` 周期性拉取 API Key 并自动入库。默认关闭 —— 升级后不应凭空
/// 开始向未知地址发请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyPullConfig {
    /// 是否启用轮询
    #[serde(default)]
    pub enabled: bool,

    /// 拉取链接（含鉴权参数）
    ///
    /// 内含密钥，日志与面板回显一律经 `key_puller::redact_url` 隐去 query。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// 轮询间隔（秒），默认 10
    #[serde(default = "default_key_pull_interval_secs")]
    pub interval_secs: u64,
}

/// 轮询间隔下限（秒）
///
/// 防止配置成 0 导致忙循环把对方接口打爆。
pub const KEY_PULL_MIN_INTERVAL_SECS: u64 = 5;

fn default_key_pull_interval_secs() -> u64 {
    10
}

impl Default for KeyPullConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: None,
            interval_secs: default_key_pull_interval_secs(),
        }
    }
}

impl KeyPullConfig {
    /// 实际生效的轮询间隔，已夹取到下限
    pub fn effective_interval_secs(&self) -> u64 {
        self.interval_secs.max(KEY_PULL_MIN_INTERVAL_SECS)
    }

    /// 是否可以真正开始轮询（启用且有非空 URL）
    pub fn is_runnable(&self) -> bool {
        self.enabled && self.url.as_deref().is_some_and(|u| !u.trim().is_empty())
    }
}

/// Prompt cache 模拟与指纹追踪配置
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheSimulationConfig {
    /// 是否启用指纹追踪（替代 from_ratio_config 末层兜底）
    #[serde(default = "default_fingerprint_enabled")]
    pub fingerprint_enabled: bool,

    /// 5m ephemeral TTL（秒）
    #[serde(default = "default_fingerprint_ttl_5m")]
    pub fingerprint_ttl_5m: u64,

    /// 1h ephemeral TTL（秒）
    #[serde(default = "default_fingerprint_ttl_1h")]
    pub fingerprint_ttl_1h: u64,

    /// 新建 cache_creation 中 1h tier 占比（0.0~1.0，默认 0.0 全部 5m）
    #[serde(default = "default_ephemeral_1h_ratio")]
    pub ephemeral_1h_ratio: f64,

    /// 单账号指纹断点上限（超出按 LRU 淘汰）
    #[serde(default = "default_fingerprint_max_breakpoints")]
    pub fingerprint_max_breakpoints_per_account: usize,
}

fn default_fingerprint_enabled() -> bool {
    true
}
fn default_fingerprint_ttl_5m() -> u64 {
    300
}
fn default_fingerprint_ttl_1h() -> u64 {
    3600
}
fn default_ephemeral_1h_ratio() -> f64 {
    0.0
}
fn default_fingerprint_max_breakpoints() -> usize {
    256
}

impl Default for CacheSimulationConfig {
    fn default() -> Self {
        Self {
            fingerprint_enabled: default_fingerprint_enabled(),
            fingerprint_ttl_5m: default_fingerprint_ttl_5m(),
            fingerprint_ttl_1h: default_fingerprint_ttl_1h(),
            ephemeral_1h_ratio: default_ephemeral_1h_ratio(),
            fingerprint_max_breakpoints_per_account: default_fingerprint_max_breakpoints(),
        }
    }
}

/// KNA 应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_region")]
    pub region: String,

    /// Auth Region（用于 Token 刷新），未配置时回退到 region
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_region: Option<String>,

    /// API Region（用于 API 请求），未配置时回退到 region
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_region: Option<String>,

    #[serde(default = "default_kiro_version")]
    pub kiro_version: String,

    #[serde(default)]
    pub machine_id: Option<String>,

    #[serde(default = "default_system_version")]
    pub system_version: String,

    #[serde(default = "default_node_version")]
    pub node_version: String,

    #[serde(default = "default_tls_backend")]
    pub tls_backend: TlsBackend,

    /// 外部 count_tokens API 地址（可选）
    #[serde(default)]
    pub count_tokens_api_url: Option<String>,

    /// count_tokens API 密钥（可选）
    #[serde(default)]
    pub count_tokens_api_key: Option<String>,

    /// count_tokens API 认证类型（可选，"x-api-key" 或 "bearer"，默认 "x-api-key"）
    #[serde(default = "default_count_tokens_auth_type")]
    pub count_tokens_auth_type: String,

    /// HTTP 代理地址（可选）
    /// 支持格式: http://host:port, https://host:port, socks5://host:port
    #[serde(default)]
    pub proxy_url: Option<String>,

    /// 代理认证用户名（可选）
    #[serde(default)]
    pub proxy_username: Option<String>,

    /// 代理认证密码（可选）
    #[serde(default)]
    pub proxy_password: Option<String>,

    /// Admin Password（可选，启用 Admin API 功能）
    #[serde(default, alias = "adminApiKey")]
    pub admin_psw: Option<String>,

    /// 负载均衡模式（"priority" 或 "balanced"）
    #[serde(default = "default_load_balancing_mode")]
    pub load_balancing_mode: String,

    /// 单账号每分钟最大请求数（超出时排队等待），0 表示不限制
    #[serde(default = "default_max_rpm_per_credential")]
    pub max_rpm_per_credential: u32,

    /// `/v1/models` 动态列表缓存 TTL（秒），默认 3600
    #[serde(default = "default_model_cache_ttl_secs")]
    pub model_cache_ttl_secs: u64,

    /// Prompt cache 模拟与指纹追踪配置
    #[serde(default)]
    pub cache_simulation: CacheSimulationConfig,

    /// 自动拉取 API Key 配置
    #[serde(default)]
    pub key_pull: KeyPullConfig,

    /// 配置文件路径（运行时元数据，不写入 JSON）
    #[serde(skip)]
    config_path: Option<PathBuf>,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_region() -> String {
    "us-east-1".to_string()
}

fn default_kiro_version() -> String {
    "2.2.2".to_string()
}

// 0 = 关闭单账号 RPM 硬限（仅依赖上游 429 + throttle_delay 兜底）
fn default_max_rpm_per_credential() -> u32 {
    0
}

fn default_system_version() -> String {
    "darwin#24.6.0".to_string()
}

fn default_node_version() -> String {
    "22.21.1".to_string()
}

fn default_count_tokens_auth_type() -> String {
    "x-api-key".to_string()
}

fn default_tls_backend() -> TlsBackend {
    TlsBackend::Rustls
}

fn default_load_balancing_mode() -> String {
    "priority".to_string()
}

fn default_model_cache_ttl_secs() -> u64 {
    3600
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            region: default_region(),
            auth_region: None,
            api_region: None,
            kiro_version: default_kiro_version(),
            machine_id: None,
            system_version: default_system_version(),
            node_version: default_node_version(),
            tls_backend: default_tls_backend(),
            count_tokens_api_url: None,
            count_tokens_api_key: None,
            count_tokens_auth_type: default_count_tokens_auth_type(),
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            admin_psw: None,
            load_balancing_mode: default_load_balancing_mode(),
            max_rpm_per_credential: default_max_rpm_per_credential(),
            model_cache_ttl_secs: default_model_cache_ttl_secs(),
            cache_simulation: CacheSimulationConfig::default(),
            key_pull: KeyPullConfig::default(),
            config_path: None,
        }
    }
}

impl Config {
    /// 获取默认配置文件路径
    pub fn default_config_path() -> &'static str {
        "config.json"
    }

    /// 获取有效的 Auth Region（用于 Token 刷新）
    /// 优先使用 auth_region，未配置时回退到 region
    pub fn effective_auth_region(&self) -> &str {
        self.auth_region.as_deref().unwrap_or(&self.region)
    }

    /// 获取有效的 API Region（用于 API 请求）
    /// 优先使用 api_region，未配置时回退到 region
    pub fn effective_api_region(&self) -> &str {
        self.api_region.as_deref().unwrap_or(&self.region)
    }

    /// 从文件加载配置
    pub fn load<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self {
                config_path: Some(path.to_path_buf()),
                ..Self::default()
            });
        }

        let content = fs::read_to_string(path)?;
        let mut config: Config = serde_json::from_str(&content)?;
        config.config_path = Some(path.to_path_buf());
        Ok(config)
    }

    /// 获取配置文件路径（如果有）
    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }

    /// 将当前配置写回原始配置文件
    #[allow(dead_code)]
    pub fn save(&self) -> anyhow::Result<()> {
        let path = self
            .config_path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("配置文件路径未知，无法保存配置"))?;

        let content = serde_json::to_string_pretty(self).context("序列化配置失败")?;
        fs::write(path, content)
            .with_context(|| format!("写入配置文件失败: {}", path.display()))?;
        Ok(())
    }

    /// 从环境变量覆盖配置项（用于容器化部署，如 Zeabur）
    ///
    /// 支持的环境变量:
    /// - `HOST`: 监听地址
    /// - `PORT`: 监听端口
    /// - `REGION`: AWS 区域
    /// - `AUTH_REGION`: Token 刷新区域
    /// - `API_REGION`: API 请求区域
    /// - `ADMIN_PSW`: Admin Password（未设置时回退读取 `ADMIN_API_KEY`）
    /// - `PROXY_URL`: HTTP 代理地址
    /// - `PROXY_USERNAME`: 代理用户名
    /// - `PROXY_PASSWORD`: 代理密码
    /// - `LOAD_BALANCING_MODE`: 负载均衡模式
    /// - `MODEL_CACHE_TTL_SECS`: /v1/models 动态列表缓存 TTL（秒）
    pub fn apply_env_overrides(&mut self) {
        if let Ok(v) = env::var("HOST") {
            self.host = v;
        }
        if let Ok(v) = env::var("PORT")
            && let Ok(p) = v.parse::<u16>()
        {
            self.port = p;
        }
        if let Ok(v) = env::var("REGION") {
            self.region = v;
        }
        if let Ok(v) = env::var("AUTH_REGION") {
            self.auth_region = Some(v);
        }
        if let Ok(v) = env::var("API_REGION") {
            self.api_region = Some(v);
        }
        if let Ok(v) = env::var("ADMIN_PSW") {
            self.admin_psw = Some(v);
        } else if let Ok(v) = env::var("ADMIN_API_KEY") {
            self.admin_psw = Some(v);
        }
        if let Ok(v) = env::var("PROXY_URL") {
            self.proxy_url = Some(v);
        }
        if let Ok(v) = env::var("PROXY_USERNAME") {
            self.proxy_username = Some(v);
        }
        if let Ok(v) = env::var("PROXY_PASSWORD") {
            self.proxy_password = Some(v);
        }
        if let Ok(v) = env::var("LOAD_BALANCING_MODE") {
            self.load_balancing_mode = v;
        }
        if let Ok(v) = env::var("MODEL_CACHE_TTL_SECS")
            && let Ok(n) = v.parse::<u64>()
        {
            self.model_cache_ttl_secs = n;
        }

        // CacheSimulationConfig 嵌套字段覆盖
        if let Ok(v) = env::var("CACHE_SIMULATION_FINGERPRINT_ENABLED")
            && let Ok(b) = v.parse::<bool>()
        {
            self.cache_simulation.fingerprint_enabled = b;
        }
        if let Ok(v) = env::var("CACHE_SIMULATION_FINGERPRINT_TTL_5M")
            && let Ok(n) = v.parse::<u64>()
        {
            self.cache_simulation.fingerprint_ttl_5m = n;
        }
        if let Ok(v) = env::var("CACHE_SIMULATION_FINGERPRINT_TTL_1H")
            && let Ok(n) = v.parse::<u64>()
        {
            self.cache_simulation.fingerprint_ttl_1h = n;
        }
        if let Ok(v) = env::var("CACHE_SIMULATION_EPHEMERAL_1H_RATIO")
            && let Ok(f) = v.parse::<f64>()
        {
            self.cache_simulation.ephemeral_1h_ratio = f.clamp(0.0, 1.0);
        }
        if let Ok(v) = env::var("CACHE_SIMULATION_FINGERPRINT_MAX_BREAKPOINTS")
            && let Ok(n) = v.parse::<usize>()
        {
            self.cache_simulation
                .fingerprint_max_breakpoints_per_account = n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_cache_ttl_default() {
        let config = Config::default();
        assert_eq!(config.model_cache_ttl_secs, 3600);
    }

    #[test]
    fn test_model_cache_ttl_deserialize_default() {
        // 配置缺省该字段时应回退默认 3600
        let config: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(config.model_cache_ttl_secs, 3600);
    }

    #[test]
    fn test_model_cache_ttl_deserialize_explicit() {
        let config: Config = serde_json::from_str(r#"{"modelCacheTtlSecs": 60}"#).unwrap();
        assert_eq!(config.model_cache_ttl_secs, 60);
    }

    #[test]
    fn test_key_pull_absent_in_old_config_stays_disabled() {
        // 关键保证：升级后旧 config.json（无 keyPull 段）不得凭空开始
        // 向未知地址发请求
        let config: Config = serde_json::from_str(r#"{"port": 5678}"#).unwrap();
        assert!(!config.key_pull.enabled);
        assert!(config.key_pull.url.is_none());
        assert!(!config.key_pull.is_runnable());
        assert_eq!(config.key_pull.interval_secs, 10);
    }

    #[test]
    fn test_key_pull_deserialize_explicit() {
        let json = r#"{"keyPull":{"enabled":true,"url":"https://h/get?t=1","intervalSecs":30}}"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert!(config.key_pull.enabled);
        assert_eq!(config.key_pull.url.as_deref(), Some("https://h/get?t=1"));
        assert_eq!(config.key_pull.effective_interval_secs(), 30);
        assert!(config.key_pull.is_runnable());
    }

    #[test]
    fn test_key_pull_interval_clamped_to_floor() {
        // 0 会导致忙循环把对方接口打爆
        let json = r#"{"keyPull":{"enabled":true,"url":"https://h/g","intervalSecs":0}}"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(
            config.key_pull.effective_interval_secs(),
            KEY_PULL_MIN_INTERVAL_SECS
        );
    }

    #[test]
    fn test_key_pull_not_runnable_without_url() {
        // enabled 但 URL 缺失/空白：不可运行，避免每轮拿 None 去请求
        let json = r#"{"keyPull":{"enabled":true}}"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert!(config.key_pull.enabled);
        assert!(!config.key_pull.is_runnable());

        let json2 = r#"{"keyPull":{"enabled":true,"url":"   "}}"#;
        let config2: Config = serde_json::from_str(json2).unwrap();
        assert!(!config2.key_pull.is_runnable());
    }

    #[test]
    fn test_key_pull_url_omitted_from_json_when_none() {
        // 未配置时不应在 config.json 里凭空出现 "url": null
        let config = Config::default();
        let json = serde_json::to_string(&config.key_pull).unwrap();
        assert!(!json.contains("url"), "实际: {}", json);
    }
}

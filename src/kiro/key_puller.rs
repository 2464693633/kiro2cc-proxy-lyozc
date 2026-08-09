// Copyright (c) 2026 Harllan He. Licensed under MIT.
//! 自动拉取 Kiro API Key
//!
//! 从用户配置的 HTTP 链接周期性拉取 API Key，解析后经
//! `MultiTokenManager::add_credential` 入库（复用其去重与校验）。
//!
//! # 为什么解析器要容忍多种形状
//!
//! 发 key 的服务不属于本项目，其响应结构未知且可能变化。硬编码一种形状
//! 猜错就永远拉不到。故采用递归下降：在每一层查找 key 样字段，命中后在
//! **同一层**配对 region，再向下递归对象与数组。这样
//! `{apiKey,region}`、`{data:{key,region}}`、`[{...},{...}]` 都能吃。
//!
//! # 为什么 region 只在同层配对
//!
//! 跨层取 region 会错配——外层的 region 可能属于另一个 key，或是无关字段。
//! 宁可 region 缺失（回退全局配置）也不给错值。

use parking_lot::RwLock;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;

use crate::http_client::{ProxyConfig, build_client};
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::token_manager::MultiTokenManager;
use crate::model::config::{KeyPullConfig, TlsBackend};

/// 从响应中解析出的单个 key
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledKey {
    pub api_key: String,
    /// 缺失时入库留空，由全局配置的 region 兜底
    pub region: Option<String>,
}

/// key 字段候选名，按特异性从高到低
///
/// `token` 放最后：它可能指别的东西（分页 token 等），只在没有更明确字段时才用。
const KEY_FIELDS: &[&str] = &["kiroApiKey", "apiKey", "api_key", "key", "token"];

/// region 字段候选名
const REGION_FIELDS: &[&str] = &["region", "apiRegion", "api_region"];

/// 在一个 JSON 对象里查找 key 样字段（返回首个命中的非空字符串值）
fn find_key_in_object(obj: &serde_json::Map<String, Value>) -> Option<String> {
    for name in KEY_FIELDS {
        if let Some(s) = obj.get(*name).and_then(|v| v.as_str())
            && !s.trim().is_empty()
        {
            return Some(s.trim().to_string());
        }
    }
    None
}

/// 在同一层查找 region
fn find_region_in_object(obj: &serde_json::Map<String, Value>) -> Option<String> {
    for name in REGION_FIELDS {
        if let Some(s) = obj.get(*name).and_then(|v| v.as_str())
            && !s.trim().is_empty()
        {
            return Some(s.trim().to_string());
        }
    }
    None
}

/// 递归收集所有 key
fn collect(value: &Value, out: &mut Vec<PulledKey>) {
    match value {
        Value::Object(obj) => {
            if let Some(api_key) = find_key_in_object(obj) {
                out.push(PulledKey {
                    api_key,
                    region: find_region_in_object(obj),
                });
                // 本层已命中即不再深入：避免把嵌套的同名字段重复计入
                return;
            }
            for v in obj.values() {
                collect(v, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                collect(v, out);
            }
        }
        _ => {}
    }
}

/// 解析拉取响应，返回去重后的 key 列表（保留首次出现的 region）
pub fn parse_pull_response(body: &str) -> anyhow::Result<Vec<PulledKey>> {
    let value: Value =
        serde_json::from_str(body).map_err(|e| anyhow::anyhow!("响应不是合法 JSON: {}", e))?;

    let mut all = Vec::new();
    collect(&value, &mut all);

    let mut seen = HashSet::new();
    let deduped: Vec<PulledKey> = all
        .into_iter()
        .filter(|k| seen.insert(k.api_key.clone()))
        .collect();

    Ok(deduped)
}

/// 隐去 URL 的整个 query 串，用于日志与面板回显
///
/// 不用首尾保留式脱敏：密钥通常整个藏在 query 里，保留首尾既泄露片段又看不出
/// 是哪个端点。这里保留 scheme/host/path，query 整体替换。
///
/// 非 ASCII 直接整串隐去——按字节切片会落在 UTF-8 边界内 panic。
pub fn redact_url(url: &str) -> String {
    if !url.is_ascii() {
        return "<已隐藏>".to_string();
    }
    match url.split_once('?') {
        Some((base, _)) => format!("{base}?<已隐藏>"),
        None => url.to_string(),
    }
}

/// 脱敏展示一个 key（前 4 + ... + 后 4），供 test 端点预览
pub fn mask_key_for_display(key: &str) -> String {
    if key.is_ascii() && key.len() > 16 {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    } else {
        "***".to_string()
    }
}

/// 单次拉取的结果统计
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PollOutcome {
    /// 响应中解析出的 key 总数
    pub parsed: usize,
    /// 新入库数
    pub added: usize,
    /// 因已存在被去重拒绝数
    pub duplicates: usize,
    /// 其它原因失败数
    pub failed: usize,
}

/// 判断 `add_credential` 的错误是否为"已存在"
///
/// 10 秒轮询下对方通常重复返回同一个 key，去重拒绝是**常态**而非异常。
/// 必须与真失败区分：前者记 debug，后者记 warn，否则日志页会被刷满。
fn is_duplicate_error(err: &anyhow::Error) -> bool {
    err.to_string().contains("账号已存在")
}

/// 把解析出的 key 构造成 API Key 凭据
///
/// region 写入 `api_region` 而非 `region` —— 前者是 API 请求实际使用的字段，
/// 与手动添加 API Key 账号的语义一致；缺失则留空由全局配置兜底。
fn build_credential(pulled: &PulledKey) -> KiroCredentials {
    KiroCredentials {
        kiro_api_key: Some(pulled.api_key.clone()),
        auth_method: Some("api_key".to_string()),
        api_region: pulled.region.clone(),
        ..Default::default()
    }
}

/// API Key 自动拉取器
pub struct KeyPuller {
    config: Arc<RwLock<KeyPullConfig>>,
    token_manager: Arc<MultiTokenManager>,
    proxy: Option<ProxyConfig>,
    tls_backend: TlsBackend,
}

impl KeyPuller {
    pub fn new(
        config: KeyPullConfig,
        token_manager: Arc<MultiTokenManager>,
        proxy: Option<ProxyConfig>,
        tls_backend: TlsBackend,
    ) -> Arc<Self> {
        Arc::new(Self {
            config: Arc::new(RwLock::new(config)),
            token_manager,
            proxy,
            tls_backend,
        })
    }

    /// 读取当前配置快照
    pub fn config(&self) -> KeyPullConfig {
        self.config.read().clone()
    }

    /// 运行时替换配置（Admin API 修改后立即生效）
    pub fn set_config(&self, new_config: KeyPullConfig) {
        *self.config.write() = new_config;
    }

    /// 启动后台轮询
    ///
    /// 无条件启动：配置可能在运行期经 Admin API 打开。未启用时每次 tick
    /// 直接跳过，不发请求也不记日志。
    pub fn start_background_poll(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut current_interval = {
                let Some(this) = weak.upgrade() else { return };
                this.config().effective_interval_secs()
            };
            let mut ticker =
                tokio::time::interval(std::time::Duration::from_secs(current_interval));
            ticker.tick().await; // 首次立即返回，跳过

            loop {
                ticker.tick().await;
                let Some(this) = weak.upgrade() else { break };

                let cfg = this.config();

                // 间隔变更后重建 ticker，使面板改动立即生效而非等旧周期走完
                let wanted = cfg.effective_interval_secs();
                if wanted != current_interval {
                    current_interval = wanted;
                    ticker = tokio::time::interval(std::time::Duration::from_secs(wanted));
                    ticker.tick().await;
                    tracing::info!("自动拉取轮询间隔已更新为 {}s", wanted);
                }

                if !cfg.is_runnable() {
                    continue;
                }

                match this.poll_once().await {
                    Ok(outcome) => {
                        if outcome.added > 0 || outcome.failed > 0 {
                            tracing::info!(
                                "自动拉取：解析 {} 个，新增 {}，重复 {}，失败 {}",
                                outcome.parsed,
                                outcome.added,
                                outcome.duplicates,
                                outcome.failed
                            );
                        } else {
                            // 全部重复是常态，debug 级别避免刷满日志
                            tracing::debug!("自动拉取：解析 {} 个，全部已存在", outcome.parsed);
                        }
                    }
                    Err(e) => tracing::warn!("自动拉取失败: {}", e),
                }
            }
        });
    }

    /// 请求并解析，不入库（供 test 端点与 poll_once 共用）
    async fn fetch_keys(&self) -> anyhow::Result<Vec<PulledKey>> {
        let cfg = self.config();
        let url = cfg
            .url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .ok_or_else(|| anyhow::anyhow!("未配置拉取链接"))?
            .to_string();

        let client = build_client(self.proxy.as_ref(), 15, self.tls_backend)?;
        let response = client.get(&url).send().await.map_err(|e| {
            // 错误信息里不能带完整 URL —— 它含密钥
            anyhow::anyhow!("请求 {} 失败: {}", redact_url(&url), e)
        })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            let preview: String = body.chars().take(200).collect();
            anyhow::bail!("{} 返回 {}: {}", redact_url(&url), status, preview);
        }

        parse_pull_response(&body)
    }

    /// 拉取一次并入库
    pub async fn poll_once(&self) -> anyhow::Result<PollOutcome> {
        let keys = self.fetch_keys().await?;
        let mut outcome = PollOutcome {
            parsed: keys.len(),
            ..Default::default()
        };

        for pulled in &keys {
            match self
                .token_manager
                .add_credential(build_credential(pulled))
                .await
            {
                Ok(id) => {
                    outcome.added += 1;
                    tracing::info!(
                        "自动拉取新增账号 #{}（{}，region={}）",
                        id,
                        mask_key_for_display(&pulled.api_key),
                        pulled.region.as_deref().unwrap_or("<全局默认>")
                    );
                }
                Err(e) if is_duplicate_error(&e) => {
                    outcome.duplicates += 1;
                    tracing::debug!(
                        "自动拉取跳过已存在的 {}",
                        mask_key_for_display(&pulled.api_key)
                    );
                }
                Err(e) => {
                    outcome.failed += 1;
                    tracing::warn!(
                        "自动拉取入库失败 {}: {}",
                        mask_key_for_display(&pulled.api_key),
                        e
                    );
                }
            }
        }

        Ok(outcome)
    }

    /// 试拉一次，只解析不入库（供 Admin test 端点验证格式）
    pub async fn dry_run(&self) -> anyhow::Result<Vec<PulledKey>> {
        self.fetch_keys().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_credential_maps_region_to_api_region() {
        let c = build_credential(&PulledKey {
            api_key: "ksk_abc".to_string(),
            region: Some("eu-central-1".to_string()),
        });
        assert_eq!(c.kiro_api_key.as_deref(), Some("ksk_abc"));
        assert_eq!(c.auth_method.as_deref(), Some("api_key"));
        assert_eq!(c.api_region.as_deref(), Some("eu-central-1"));
        assert!(c.is_api_key_credential());
    }

    #[test]
    fn build_credential_leaves_region_empty_when_absent() {
        // region 缺失应留空由全局配置兜底，而非填死某个值
        let c = build_credential(&PulledKey {
            api_key: "ksk_abc".to_string(),
            region: None,
        });
        assert_eq!(c.api_region, None);
    }

    #[test]
    fn distinguishes_duplicate_from_real_failure() {
        // 去重是 10s 轮询下的常态，必须与真失败分流，否则日志被刷满
        assert!(is_duplicate_error(&anyhow::anyhow!(
            "账号已存在（kiroApiKey 重复）"
        )));
        assert!(!is_duplicate_error(&anyhow::anyhow!("kiroApiKey 为空")));
        assert!(!is_duplicate_error(&anyhow::anyhow!("网络错误")));
    }

    #[test]
    fn parses_flat_object() {
        let keys = parse_pull_response(r#"{"apiKey":"ksk_abc","region":"us-east-1"}"#).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].api_key, "ksk_abc");
        assert_eq!(keys[0].region.as_deref(), Some("us-east-1"));
    }

    #[test]
    fn parses_nested_under_data() {
        let body = r#"{"code":0,"msg":"ok","data":{"key":"ksk_nested","region":"eu-central-1"}}"#;
        let keys = parse_pull_response(body).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].api_key, "ksk_nested");
        assert_eq!(keys[0].region.as_deref(), Some("eu-central-1"));
    }

    #[test]
    fn parses_array_of_objects() {
        let body = r#"[{"apiKey":"ksk_a","region":"us-east-1"},{"apiKey":"ksk_b","region":"eu-central-1"}]"#;
        let keys = parse_pull_response(body).unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].api_key, "ksk_a");
        assert_eq!(keys[1].region.as_deref(), Some("eu-central-1"));
    }

    #[test]
    fn parses_array_nested_under_data() {
        let body = r#"{"data":{"list":[{"kiroApiKey":"ksk_x"},{"kiroApiKey":"ksk_y"}]}}"#;
        let keys = parse_pull_response(body).unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].api_key, "ksk_x");
        assert_eq!(keys[1].api_key, "ksk_y");
    }

    #[test]
    fn field_name_priority_prefers_specific() {
        // 同层同时有 token 与 apiKey 时取 apiKey（token 可能指别的东西）
        let body = r#"{"token":"pagination_cursor","apiKey":"ksk_real"}"#;
        let keys = parse_pull_response(body).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].api_key, "ksk_real");
    }

    #[test]
    fn accepts_token_field_when_no_better_candidate() {
        let keys = parse_pull_response(r#"{"token":"ksk_from_token"}"#).unwrap();
        assert_eq!(keys[0].api_key, "ksk_from_token");
    }

    #[test]
    fn region_only_pairs_from_same_level() {
        // 回归重点：外层 region 不得配给内层 key —— 那个 region 可能属于别的东西
        let body = r#"{"region":"outer-region","data":{"apiKey":"ksk_inner"}}"#;
        let keys = parse_pull_response(body).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].api_key, "ksk_inner");
        assert_eq!(keys[0].region, None, "region 只能同层配对，跨层取会错配");
    }

    #[test]
    fn dedups_repeated_keys_keeping_first_region() {
        let body = r#"[{"apiKey":"ksk_same","region":"us-east-1"},{"apiKey":"ksk_same","region":"eu-central-1"}]"#;
        let keys = parse_pull_response(body).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].region.as_deref(), Some("us-east-1"));
    }

    #[test]
    fn skips_empty_and_whitespace_values() {
        let keys = parse_pull_response(r#"{"apiKey":"","key":"   ","token":"ksk_ok"}"#).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].api_key, "ksk_ok");
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let keys =
            parse_pull_response(r#"{"apiKey":"  ksk_padded  ","region":" us-east-1 "}"#).unwrap();
        assert_eq!(keys[0].api_key, "ksk_padded");
        assert_eq!(keys[0].region.as_deref(), Some("us-east-1"));
    }

    #[test]
    fn returns_empty_when_no_key_field_present() {
        let keys = parse_pull_response(r#"{"code":500,"msg":"quota exhausted"}"#).unwrap();
        assert!(keys.is_empty(), "无 key 字段应返回空列表而非报错");
    }

    #[test]
    fn errors_on_invalid_json() {
        assert!(parse_pull_response("not json at all").is_err());
        assert!(parse_pull_response("").is_err());
    }

    // ==================== redact_url ====================

    #[test]
    fn redact_hides_entire_query_string() {
        assert_eq!(
            redact_url("https://car.example.com/get?token=secret123"),
            "https://car.example.com/get?<已隐藏>"
        );
        // 多参数同样整体隐去
        assert_eq!(
            redact_url("https://h/p?a=1&token=s&b=2"),
            "https://h/p?<已隐藏>"
        );
    }

    #[test]
    fn redact_keeps_url_without_query_intact() {
        assert_eq!(
            redact_url("https://example.com/get"),
            "https://example.com/get"
        );
    }

    #[test]
    fn redact_handles_non_ascii_without_panic() {
        // 按字节切片会 panic，必须整串隐去
        assert_eq!(redact_url("https://例子.com/获取?token=x"), "<已隐藏>");
    }

    #[test]
    fn mask_key_for_display_matches_credentials_helper() {
        assert_eq!(
            mask_key_for_display("ksk_abcdefghijklmnop1234"),
            "ksk_...1234"
        );
        assert_eq!(mask_key_for_display("short"), "***");
        assert_eq!(mask_key_for_display("密钥密钥密钥密钥密钥"), "***");
    }
}

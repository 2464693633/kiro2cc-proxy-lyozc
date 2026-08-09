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

use serde_json::Value;
use std::collections::HashSet;

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

#[cfg(test)]
mod tests {
    use super::*;

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

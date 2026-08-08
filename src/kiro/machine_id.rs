// Copyright (c) 2026 Harllan He. Licensed under MIT.
//! 设备指纹生成器
//!

use sha2::{Digest, Sha256};

use crate::kiro::model::credentials::KiroCredentials;
use crate::model::config::Config;

/// 标准化 machineId 格式
///
/// 支持以下格式：
/// - 64 字符十六进制字符串（直接返回）
/// - UUID 格式（如 "2582956e-cc88-4669-b546-07adbffcb894"，移除连字符后补齐到 64 字符）
fn normalize_machine_id(machine_id: &str) -> Option<String> {
    let trimmed = machine_id.trim();

    // 如果已经是 64 字符，直接返回
    if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(trimmed.to_string());
    }

    // 尝试解析 UUID 格式（移除连字符）
    let without_dashes: String = trimmed.chars().filter(|c| *c != '-').collect();

    // UUID 去掉连字符后是 32 字符
    if without_dashes.len() == 32 && without_dashes.chars().all(|c| c.is_ascii_hexdigit()) {
        // 补齐到 64 字符（重复一次）
        return Some(format!("{}{}", without_dashes, without_dashes));
    }

    // 无法识别的格式
    None
}

/// 根据凭证信息生成唯一的 Machine ID
///
/// 优先使用账号级 machineId，其次使用 config.machineId，然后使用 refreshToken 生成
pub fn generate_from_credentials(credentials: &KiroCredentials, config: &Config) -> Option<String> {
    // 如果配置了账号级 machineId，优先使用
    if let Some(ref machine_id) = credentials.machine_id
        && let Some(normalized) = normalize_machine_id(machine_id)
    {
        return Some(normalized);
    }

    // 如果配置了全局 machineId，作为默认值
    if let Some(ref machine_id) = config.machine_id
        && let Some(normalized) = normalize_machine_id(machine_id)
    {
        return Some(normalized);
    }

    // 按凭据类型派生。两条路径互斥、不互相回落：
    // API Key 凭据没有 refreshToken，OAuth 凭据没有 kiroApiKey，
    // 若允许回落会让同一账号在不同调用间产生不同 machineId。
    if credentials.is_api_key_credential() {
        if let Some(ref api_key) = credentials.kiro_api_key
            && !api_key.is_empty()
        {
            return Some(sha256_hex(&format!("KiroAPIKey/{}", api_key)));
        }
    } else if let Some(ref refresh_token) = credentials.refresh_token
        && !refresh_token.is_empty()
    {
        return Some(sha256_hex(&format!("KotlinNativeAPI/{}", refresh_token)));
    }

    // 没有有效的凭证
    None
}

/// SHA256 哈希实现（返回十六进制字符串）
fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_hex() {
        let result = sha256_hex("test");
        assert_eq!(result.len(), 64);
        assert_eq!(
            result,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
    }

    #[test]
    fn test_generate_with_custom_machine_id() {
        let credentials = KiroCredentials::default();
        let mut config = Config::default();
        config.machine_id = Some("a".repeat(64));

        let result = generate_from_credentials(&credentials, &config);
        assert_eq!(result, Some("a".repeat(64)));
    }

    #[test]
    fn test_generate_with_credential_machine_id_overrides_config() {
        let mut credentials = KiroCredentials::default();
        credentials.machine_id = Some("b".repeat(64));

        let mut config = Config::default();
        config.machine_id = Some("a".repeat(64));

        let result = generate_from_credentials(&credentials, &config);
        assert_eq!(result, Some("b".repeat(64)));
    }

    #[test]
    fn test_generate_with_refresh_token() {
        let mut credentials = KiroCredentials::default();
        credentials.refresh_token = Some("test_refresh_token".to_string());
        let config = Config::default();

        let result = generate_from_credentials(&credentials, &config);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().len(), 64);
    }

    #[test]
    fn test_generate_from_api_key_uses_distinct_seed() {
        let mut api = KiroCredentials::default();
        api.kiro_api_key = Some("ksk_test".to_string());
        let config = Config::default();

        let from_api = generate_from_credentials(&api, &config).unwrap();
        assert_eq!(from_api.len(), 64);
        // 与同值 refreshToken 的派生结果必须不同（种子前缀不同）
        let mut oauth = KiroCredentials::default();
        oauth.refresh_token = Some("ksk_test".to_string());
        let from_rt = generate_from_credentials(&oauth, &config).unwrap();
        assert_ne!(
            from_api, from_rt,
            "API Key 与 refreshToken 派生必须使用不同种子"
        );
        // 稳定性：同一 key 多次派生一致
        assert_eq!(from_api, generate_from_credentials(&api, &config).unwrap());
    }

    #[test]
    fn test_api_key_credential_does_not_fall_back_to_refresh_token() {
        // 两条路径互斥：声明 api_key 但 key 为空时，不得回落到 refreshToken 派生，
        // 否则同一账号在配置修好前后会得到不同 machineId
        let mut cred = KiroCredentials::default();
        cred.auth_method = Some("api_key".to_string());
        cred.refresh_token = Some("some_refresh_token".to_string());
        // kiro_api_key 保持 None

        let config = Config::default();
        assert!(
            generate_from_credentials(&cred, &config).is_none(),
            "API Key 凭据不应回落到 refreshToken 派生"
        );
    }

    #[test]
    fn test_generate_without_credentials() {
        let credentials = KiroCredentials::default();
        let config = Config::default();

        let result = generate_from_credentials(&credentials, &config);
        assert!(result.is_none());
    }

    #[test]
    fn test_normalize_uuid_format() {
        // UUID 格式应该被转换为 64 字符
        let uuid = "2582956e-cc88-4669-b546-07adbffcb894";
        let result = normalize_machine_id(uuid);
        assert!(result.is_some());
        let normalized = result.unwrap();
        assert_eq!(normalized.len(), 64);
        // UUID 去掉连字符后重复一次
        assert_eq!(
            normalized,
            "2582956ecc884669b54607adbffcb8942582956ecc884669b54607adbffcb894"
        );
    }

    #[test]
    fn test_normalize_64_char_hex() {
        // 64 字符十六进制应该直接返回
        let hex64 = "a".repeat(64);
        let result = normalize_machine_id(&hex64);
        assert_eq!(result, Some(hex64));
    }

    #[test]
    fn test_normalize_invalid_format() {
        // 无效格式应该返回 None
        assert!(normalize_machine_id("invalid").is_none());
        assert!(normalize_machine_id("too-short").is_none());
        assert!(normalize_machine_id(&"g".repeat(64)).is_none()); // 非十六进制
    }

    #[test]
    fn test_generate_with_uuid_machine_id() {
        let mut credentials = KiroCredentials::default();
        credentials.machine_id = Some("2582956e-cc88-4669-b546-07adbffcb894".to_string());

        let config = Config::default();

        let result = generate_from_credentials(&credentials, &config);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().len(), 64);
    }
}

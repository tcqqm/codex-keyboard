use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};

use crate::paths::AppPaths;
use crate::store::JobFailureKind;

pub fn run_turn(provider: &str, directory: &str, prompt: &str) -> Result<String, JobFailureKind> {
    let key = read_key(provider).ok_or(JobFailureKind::Authentication)?;
    let body = request_body(provider, directory, prompt).ok_or(JobFailureKind::InvalidOutput)?;
    let response = post_json(provider, &key, &body)?;
    parse_reply(provider, &response).ok_or(JobFailureKind::InvalidOutput)
}

fn read_key(provider: &str) -> Option<String> {
    let saved = read_provider_file(provider);
    if provider != "grok" {
        return saved;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    // 保存的 Grok 登录是几小时就过期的 JWT。过期后改用本机 CLI 里仍有效的那份。
    prefer_grok_credential(saved, read_grok_cli_key(), now)
}

fn read_provider_file(provider: &str) -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let path = AppPaths::from_home(Path::new(&home))
        .root
        .join("providers")
        .join(format!("{provider}.key"));
    let text = fs::read_to_string(path).ok()?;
    let key = text.trim();
    if key.is_empty() {
        None
    } else {
        Some(key.to_owned())
    }
}

fn read_grok_cli_key() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let raw = fs::read_to_string(Path::new(&home).join(".grok").join("auth.json")).ok()?;
    grok_token_from_cli_auth(&raw)
}

/// 文件里的长期密钥优先。JWT 已过期时才用 CLI 里未过期的登录。
fn prefer_grok_credential(
    saved: Option<String>,
    cli: Option<String>,
    now_unix: u64,
) -> Option<String> {
    if saved
        .as_deref()
        .is_some_and(|token| credential_usable(token, now_unix))
    {
        return saved;
    }
    if cli
        .as_deref()
        .is_some_and(|token| credential_usable(token, now_unix))
    {
        return cli;
    }
    saved.or(cli)
}

fn credential_usable(token: &str, now_unix: u64) -> bool {
    match jwt_expiry_unix(token) {
        Some(exp) => exp > now_unix.saturating_add(30),
        None => true,
    }
}

fn jwt_expiry_unix(token: &str) -> Option<u64> {
    let mut parts = token.split('.');
    let header = parts.next()?;
    let payload = parts.next()?;
    let signature = parts.next()?;
    if parts.next().is_some() || header.is_empty() || signature.is_empty() {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    Some(value.get("exp").and_then(Value::as_u64).unwrap_or(0))
}

fn grok_token_from_cli_auth(raw: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let accounts = value.as_object()?;
    let mut best_exp = 0_u64;
    let mut best: Option<String> = None;
    let mut opaque: Option<String> = None;
    for account in accounts.values() {
        let Some(key) = account.get("key").and_then(Value::as_str) else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        match jwt_expiry_unix(key) {
            Some(exp) if exp >= best_exp => {
                best_exp = exp;
                best = Some(key.to_owned());
            }
            Some(_) => {}
            None if opaque.is_none() => opaque = Some(key.to_owned()),
            None => {}
        }
    }
    best.or(opaque)
}

fn request_body(provider: &str, directory: &str, prompt: &str) -> Option<String> {
    let system = format!("你是键盘上的语音助手。用户正在文件夹 {directory} 里工作。直接回答。");
    let body = match provider {
        "grok" => json!({
            "model": "grok-4",
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": prompt}
            ]
        }),
        "claude" => json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 1024,
            "system": system,
            "messages": [{"role": "user", "content": prompt}]
        }),
        "deepseek" => json!({
            "model": "deepseek-v4-pro",
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": prompt}
            ]
        }),
        _ => return None,
    };
    Some(body.to_string())
}

fn endpoint(provider: &str) -> Option<String> {
    match provider {
        "grok" => Some("https://api.x.ai/v1/chat/completions".to_owned()),
        "claude" => claude_messages_url(),
        "deepseek" => Some("https://api.deepseek.com/chat/completions".to_owned()),
        _ => None,
    }
}

/// 缺省走 Anthropic 官方。本机如果写了 `providers/claude.base`，就用那一行的地址。
/// 文件存在但内容不合法时拒绝，避免把密钥发到另一个站点。
fn claude_messages_url() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let path = AppPaths::from_home(Path::new(&home))
        .root
        .join("providers")
        .join("claude.base");
    match fs::read_to_string(path) {
        Ok(raw) => {
            let base = raw.trim();
            if base.is_empty() {
                Some(CLAUDE_DEFAULT_MESSAGES_URL.to_owned())
            } else {
                messages_url_from_base(base)
            }
        }
        Err(_) => Some(CLAUDE_DEFAULT_MESSAGES_URL.to_owned()),
    }
}

const CLAUDE_DEFAULT_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";

fn messages_url_from_base(base: &str) -> Option<String> {
    let trimmed = base.trim().trim_end_matches('/');
    let origin = trimmed.strip_suffix("/v1/messages").unwrap_or(trimmed);
    if !valid_https_origin(origin) {
        return None;
    }
    Some(format!("{origin}/v1/messages"))
}

fn valid_https_origin(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    !rest.is_empty()
        && rest.len() <= 200
        && !rest.contains('/')
        && !rest.contains('@')
        && !rest.contains('?')
        && !rest.contains('#')
        && rest
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\'')
}

fn post_json(provider: &str, key: &str, body: &str) -> Result<String, JobFailureKind> {
    let url = endpoint(provider).ok_or(JobFailureKind::InvalidOutput)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| JobFailureKind::ProcessIo)?
        .as_nanos();
    let home = std::env::var_os("HOME").ok_or(JobFailureKind::ProcessIo)?;
    let dir = AppPaths::from_home(Path::new(&home))
        .runtime_directory
        .join("provider-calls");
    fs::create_dir_all(&dir).map_err(|_| JobFailureKind::ProcessIo)?;
    let body_path = dir.join(format!("{stamp}.json"));
    let header_path = dir.join(format!("{stamp}.headers"));
    fs::write(&body_path, body).map_err(|_| JobFailureKind::ProcessIo)?;
    let headers = match provider {
        "grok" | "deepseek" => {
            format!("Authorization: Bearer {key}\nContent-Type: application/json\n")
        }
        "claude" => format!(
            "x-api-key: {key}\nanthropic-version: 2023-06-01\nContent-Type: application/json\n"
        ),

        _ => return Err(JobFailureKind::InvalidOutput),
    };
    fs::write(&header_path, headers).map_err(|_| JobFailureKind::ProcessIo)?;
    let output = Command::new("/usr/bin/curl")
        .args([
            "-sS",
            "-m",
            "90",
            "-X",
            "POST",
            "--header",
            &format!("@{header}", header = header_path.display()),
            "--data-binary",
            &format!("@{body}", body = body_path.display()),
            url.as_str(),
        ])
        .output()
        .map_err(|_| JobFailureKind::ProcessIo)?;
    let _ = fs::remove_file(&body_path);
    let _ = fs::remove_file(&header_path);
    if !output.status.success() {
        return Err(JobFailureKind::ProcessIo);
    }
    String::from_utf8(output.stdout).map_err(|_| JobFailureKind::InvalidOutput)
}

fn parse_reply(provider: &str, response: &str) -> Option<String> {
    let value: Value = serde_json::from_str(response).ok()?;
    let text = match provider {
        "grok" | "deepseek" => value
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::to_owned),
        "claude" => value
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }?;
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        grok_token_from_cli_auth, messages_url_from_base, parse_reply, prefer_grok_credential,
        request_body,
    };

    fn jwt(exp: u64) -> String {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#).as_bytes());
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn reads_each_provider_reply() {
        assert_eq!(
            parse_reply("grok", r#"{"choices":[{"message":{"content":"你好"}}]}"#).as_deref(),
            Some("你好")
        );
        assert_eq!(
            parse_reply("claude", r#"{"content":[{"text":"好"}]}"#).as_deref(),
            Some("好")
        );
        assert_eq!(
            parse_reply(
                "deepseek",
                r#"{"choices":[{"message":{"content":"收到"}}]}"#
            )
            .as_deref(),
            Some("收到")
        );
        let body = request_body("deepseek", "/work", "hi").expect("body");
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["model"], "deepseek-v4-pro");
    }

    #[test]
    fn claude_uses_the_proxy_model_and_only_https_origins() {
        let body = request_body("claude", "/work", "hi").expect("body");
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["model"], "claude-sonnet-4-6");
        assert_eq!(
            messages_url_from_base("https://example.test").as_deref(),
            Some("https://example.test/v1/messages")
        );
        assert_eq!(
            messages_url_from_base("https://example.test/v1/messages/").as_deref(),
            Some("https://example.test/v1/messages")
        );
        assert!(messages_url_from_base("http://example.test").is_none());
        assert!(messages_url_from_base("https://user:pass@example.test").is_none());
        assert!(messages_url_from_base("https://example.test/other").is_none());
    }

    #[test]
    fn expired_grok_file_uses_the_live_cli_login() {
        let expired = jwt(1_000);
        let fresh = jwt(5_000);
        let auth = format!(r#"{{"account":{{"auth_mode":"oidc","key":"{fresh}"}}}}"#);
        assert_eq!(
            prefer_grok_credential(Some(expired), grok_token_from_cli_auth(&auth), 2_000)
                .as_deref(),
            Some(jwt(5_000).as_str())
        );
    }

    #[test]
    fn durable_grok_key_beats_the_cli_login() {
        let durable = "xai-live-key";
        let fresh = jwt(5_000);
        assert_eq!(
            prefer_grok_credential(Some(durable.to_owned()), Some(fresh), 2_000).as_deref(),
            Some(durable)
        );
    }

    #[test]
    fn cli_auth_picks_the_later_login_and_ignores_other_fields() {
        let older = jwt(3_000);
        let newer = jwt(9_000);
        let auth = format!(
            r#"{{"a":{{"key":"{older}","refresh_token":"r1"}},"b":{{"key":"{newer}","email":"hidden"}}}}"#
        );
        assert_eq!(
            grok_token_from_cli_auth(&auth).as_deref(),
            Some(newer.as_str())
        );
        assert!(grok_token_from_cli_auth(r#"{"a":{"refresh_token":"r"}}"#).is_none());
    }
}

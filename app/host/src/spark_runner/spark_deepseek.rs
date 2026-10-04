use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::SparkError;
use crate::paths::AppPaths;
use crate::summary::MAX_SUMMARY_DOCUMENT_BYTES;

/// 口播稿用的快模型。思考默认开着，不显式关掉就会先写思维链。
pub(super) const MODEL: &str = "deepseek-flash";
const URL: &str = "https://api.deepseek.com/chat/completions";
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_KEY_BYTES: u64 = 4096;

pub(super) fn summary_request_body(prompt: &str, schema: &str) -> String {
    json!({
        "model": MODEL,
        "messages": [
            {
                "role": "system",
                "content": "Reply with one json object and no markdown."
            },
            {
                "role": "user",
                "content": format!("{prompt}\nOutput schema:\n{schema}\n")
            }
        ],
        "thinking": {"type": "disabled"},
        "response_format": {"type": "json_object"},
        "max_tokens": 4096,
        "stream": false
    })
    .to_string()
}

/// 从聊天补全响应里取出 JSON 正文。思维链字段不进入口播稿。
pub(super) fn summary_document_bytes(response: &[u8]) -> Result<Vec<u8>, SparkError> {
    if response.is_empty() || response.len() > MAX_RESPONSE_BYTES {
        return Err(SparkError::InvalidOutput);
    }
    let value: Value = serde_json::from_slice(response).map_err(|_| SparkError::InvalidOutput)?;
    let finish = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str);
    if finish != Some("stop") {
        return Err(SparkError::InvalidOutput);
    }
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or(SparkError::InvalidOutput)?;
    let mut document: Value =
        serde_json::from_str(strip_json_fence(content)).map_err(|_| SparkError::InvalidOutput)?;
    let object = document.as_object_mut().ok_or(SparkError::InvalidOutput)?;
    object.retain(|key, _| {
        matches!(
            key.as_str(),
            "schema"
                | "facts"
                | "pending"
                | "decisions"
                | "spoken_text"
                | "covers_new_completions"
                | "source_evidence"
        )
    });
    let bytes = serde_json::to_vec(&document).map_err(|_| SparkError::InvalidOutput)?;
    if bytes.len() > MAX_SUMMARY_DOCUMENT_BYTES {
        return Err(SparkError::OutputTooLarge);
    }
    Ok(bytes)
}

pub(super) fn post_summary(
    prompt: &str,
    schema: &str,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, SparkError> {
    if cancel.load(Ordering::Acquire) {
        return Err(SparkError::Cancelled);
    }
    let key = read_key()?;
    let body = Zeroizing::new(summary_request_body(prompt, schema));
    let seconds = timeout.as_secs().clamp(1, 90);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SparkError::ProcessIo)?
        .as_nanos();
    let home = std::env::var_os("HOME").ok_or(SparkError::Authentication)?;
    let home = PathBuf::from(home);
    let directory = AppPaths::from_home(&home)
        .runtime_directory
        .join("provider-calls");
    fs::create_dir_all(&directory).map_err(|_| SparkError::ProcessIo)?;
    let mut cleanup = TempCall { paths: Vec::new() };
    let header_path = directory.join(format!("spark-{stamp}.headers"));
    let body_path = directory.join(format!("spark-{stamp}.json"));
    let output_path = directory.join(format!("spark-{stamp}.out"));
    let header = Zeroizing::new(format!(
        "Authorization: Bearer {key}\nContent-Type: application/json\n",
        key = key.as_str()
    ));
    write_private(&header_path, header.as_bytes())?;
    cleanup.paths.push(header_path.clone());
    write_private(&body_path, body.as_bytes())?;
    cleanup.paths.push(body_path.clone());
    cleanup.paths.push(output_path.clone());
    let seconds_text = seconds.to_string();
    let header_arg = format!("@{}", header_path.display());
    let body_arg = format!("@{}", body_path.display());
    // 密钥和稿子只在 0600 临时文件里，不出现在进程参数中。
    let mut child = Command::new("/usr/bin/curl")
        .args([
            "-sS",
            "-m",
            seconds_text.as_str(),
            "-w",
            "%{http_code}",
            "-X",
            "POST",
            "--header",
            header_arg.as_str(),
            "--data-binary",
            body_arg.as_str(),
            URL,
        ])
        .arg("-o")
        .arg(&output_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| SparkError::ProcessIo)?;
    let deadline = Instant::now() + Duration::from_secs(seconds.saturating_add(5));
    let status = loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SparkError::Cancelled);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SparkError::Timeout);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SparkError::ProcessIo);
            }
        }
    };
    let mut stdout = child.stdout.take().ok_or(SparkError::ProcessIo)?;
    let mut code_text = String::new();
    stdout
        .read_to_string(&mut code_text)
        .map_err(|_| SparkError::ProcessIo)?;
    if !status.success() && code_text.trim().is_empty() {
        return Err(SparkError::ServiceUnavailable);
    }
    let code: u16 = code_text.trim().parse().unwrap_or(0);
    let response = read_limited(&output_path)?;
    map_status(code)?;
    summary_document_bytes(&response)
}

fn map_status(code: u16) -> Result<(), SparkError> {
    match code {
        200 => Ok(()),
        401 | 403 => Err(SparkError::Authentication),
        404 => Err(SparkError::ModelUnavailable),
        408 => Err(SparkError::Timeout),
        429 => Err(SparkError::RateLimited),
        500..=599 => Err(SparkError::ServiceUnavailable),
        _ => Err(SparkError::InvalidOutput),
    }
}

fn read_key() -> Result<Zeroizing<String>, SparkError> {
    let home = std::env::var_os("HOME").ok_or(SparkError::Authentication)?;
    let home = PathBuf::from(home);
    let path = AppPaths::from_home(&home)
        .root
        .join("providers")
        .join("deepseek.key");
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| SparkError::Authentication)?;
    let metadata = file.metadata().map_err(|_| SparkError::Authentication)?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.len() == 0
        || metadata.len() > MAX_KEY_BYTES
    {
        return Err(SparkError::Authentication);
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|_| SparkError::Authentication)?;
    let key = text.trim();
    if key.len() < 16
        || key.len() > MAX_KEY_BYTES as usize
        || !key.is_ascii()
        || key.chars().any(|character| character.is_ascii_whitespace())
    {
        return Err(SparkError::Authentication);
    }
    Ok(Zeroizing::new(key.to_owned()))
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), SparkError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| SparkError::ProcessIo)?;
    file.write_all(bytes).map_err(|_| SparkError::ProcessIo)?;
    file.sync_all().map_err(|_| SparkError::ProcessIo)?;
    Ok(())
}

fn read_limited(path: &std::path::Path) -> Result<Vec<u8>, SparkError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| SparkError::ProcessIo)?;
    let length = file.metadata().map_err(|_| SparkError::ProcessIo)?.len() as usize;
    if length > MAX_RESPONSE_BYTES {
        return Err(SparkError::OutputTooLarge);
    }
    let mut bytes = Vec::with_capacity(length);
    file.read_to_end(&mut bytes)
        .map_err(|_| SparkError::ProcessIo)?;
    Ok(bytes)
}

fn strip_json_fence(value: &str) -> &str {
    let trimmed = value.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let rest = rest
        .strip_prefix("json")
        .unwrap_or(rest)
        .trim_start_matches(['\n', '\r']);
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

struct TempCall {
    paths: Vec<PathBuf>,
}

impl Drop for TempCall {
    fn drop(&mut self) {
        for path in &self.paths {
            if let Ok(file) = OpenOptions::new().write(true).open(path) {
                let _ = file.set_len(0);
            }
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{summary_document_bytes, summary_request_body};
    use crate::spark_runner::SparkError;

    #[test]
    fn request_uses_flash_without_thinking() {
        let body = summary_request_body("summarize this json input", r#"{"type":"object"}"#);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["model"], "deepseek-flash");
        assert_eq!(value["thinking"]["type"], "disabled");
        assert!(value.get("reasoning_effort").is_none());
        assert_eq!(value["response_format"]["type"], "json_object");
        assert_eq!(value["stream"], false);
        let user = value["messages"][1]["content"].as_str().unwrap();
        assert!(user.contains("summarize this json input"));
        assert!(user.contains(r#"{"type":"object"}"#));
        assert!(!user.contains("reasoning_effort"));
    }

    #[test]
    fn response_keeps_content_and_drops_extra_keys() {
        let response = br#"{"choices":[{"finish_reason":"stop","message":{"content":"```json\n{\"schema\":1,\"facts\":[\"done\"],\"pending\":[],\"decisions\":[],\"spoken_text\":\"done\",\"covers_new_completions\":[\"00000000-0000-4000-8000-000000000002\"],\"note\":\"ignore\"}\n```","reasoning_content":"hidden"}}]}"#;
        let bytes = summary_document_bytes(response).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["spoken_text"], "done");
        assert!(value.get("note").is_none());
        assert!(value.get("reasoning_content").is_none());
    }

    #[test]
    fn truncated_or_empty_content_is_rejected() {
        assert_eq!(
            summary_document_bytes(br#"{"choices":[{"finish_reason":"length","message":{"content":"{\"schema\":1}"}}]}"#),
            Err(SparkError::InvalidOutput)
        );
        assert_eq!(
            summary_document_bytes(
                br#"{"choices":[{"finish_reason":"stop","message":{"content":""}}]}"#
            ),
            Err(SparkError::InvalidOutput)
        );
    }
}

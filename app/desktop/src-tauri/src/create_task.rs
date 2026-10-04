use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

const MAX_TASK_NAME_CHARS: usize = 40;

pub fn normalize_task_name(input: &str) -> Result<String, &'static str> {
    let name = input.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = name.chars().count();
    if !(1..=MAX_TASK_NAME_CHARS).contains(&count) || name.chars().any(|ch| ch.is_control()) {
        return Err("invalid_name");
    }
    Ok(name)
}

pub fn normalize_directory(input: &str) -> Result<PathBuf, &'static str> {
    let path = PathBuf::from(input.trim());
    if !path.is_absolute() {
        return Err("invalid_directory");
    }
    let path = path.canonicalize().map_err(|_| "invalid_directory")?;
    if !path.is_dir() {
        return Err("invalid_directory");
    }
    Ok(path)
}

pub fn create_thread(name: &str, directory: &str) -> Result<String, &'static str> {
    let name = normalize_task_name(name)?;
    let directory = normalize_directory(directory)?;
    let codex = codex_executable().ok_or("codex_missing")?;
    let cwd = directory.to_string_lossy().into_owned();
    let mut server = Command::new(codex)
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "codex_failed")?;
    let mut stdin = server.stdin.take().ok_or("codex_failed")?;
    let stdout = server.stdout.take().ok_or("codex_failed")?;
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match std::io::BufRead::read_line(&mut reader, &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(line.trim().to_owned()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let task_id: Result<String, &'static str> = (|| {
        rpc(
            &mut stdin,
            &rx,
            1,
            "initialize",
            json!({
                "clientInfo": {"name": "codex-keyboard", "version": "0.1.0"},
                "capabilities": {"experimentalApi": true}
            }),
        )?;
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc": "2.0", "method": "initialized"})
        )
        .map_err(|_| "codex_failed")?;
        let started = rpc(
            &mut stdin,
            &rx,
            2,
            "thread/start",
            json!({
                "cwd": cwd,
                "ephemeral": false,
                "historyMode": "legacy"
            }),
        )?;
        let task_id = started
            .pointer("/result/thread/id")
            .and_then(Value::as_str)
            .filter(|id| is_uuid(id))
            .ok_or("thread_missing")?
            .to_owned();
        rpc(
            &mut stdin,
            &rx,
            3,
            "thread/name/set",
            json!({"threadId": task_id, "name": name}),
        )?;
        Ok(task_id)
    })();
    drop(stdin);
    let _ = server.wait_timeout(Duration::from_secs(8));
    let task_id = task_id?;
    assign_task_name(&task_id, &name)?;
    Ok(task_id)
}

fn rpc(
    stdin: &mut impl Write,
    lines: &std::sync::mpsc::Receiver<String>,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value, &'static str> {
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    )
    .map_err(|_| "codex_failed")?;
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        let line = match lines.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => line,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err("codex_failed"),
        };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if value.get("error").is_some() {
            return Err("codex_failed");
        }
        return Ok(value);
    }
    Err("codex_failed")
}

trait WaitTimeout {
    fn wait_timeout(&mut self, timeout: Duration) -> std::io::Result<()>;
}

impl WaitTimeout for std::process::Child {
    fn wait_timeout(&mut self, timeout: Duration) -> std::io::Result<()> {
        let started = Instant::now();
        loop {
            if self.try_wait()?.is_some() {
                return Ok(());
            }
            if started.elapsed() > timeout {
                let _ = self.kill();
                let _ = self.wait();
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn assign_task_name(task_id: &str, name: &str) -> Result<(), &'static str> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("home_unavailable")?);
    let codex_home = home.join(".codex");
    let connection = Connection::open_with_flags(
        codex_home.join("state_5.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "name_failed")?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .map_err(|_| "name_failed")?;
    let changed = connection
        .execute(
            "UPDATE threads SET name = ?1, title = ?1 WHERE id = ?2",
            (name, task_id),
        )
        .map_err(|_| "name_failed")?;
    if changed != 1 {
        return Err("name_failed");
    }
    append_session_name(&codex_home.join("session_index.jsonl"), task_id, name)
}

fn append_session_name(path: &Path, task_id: &str, name: &str) -> Result<(), &'static str> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)
        .map_err(|_| "name_failed")?;
    let length = file.metadata().map_err(|_| "name_failed")?.len();
    if length > 0 {
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::End(-1)).map_err(|_| "name_failed")?;
        let mut last = [0_u8; 1];
        file.read_exact(&mut last).map_err(|_| "name_failed")?;
        if last[0] != b'\n' {
            file.write_all(b"\n").map_err(|_| "name_failed")?;
        }
    }
    let line = json!({"id": task_id, "thread_name": name});
    writeln!(file, "{line}").map_err(|_| "name_failed")?;
    Ok(())
}

fn codex_executable() -> Option<PathBuf> {
    if let Some(configured) = std::env::var_os("EASY_CODEX_CLI") {
        let path = PathBuf::from(configured);
        if path.is_absolute() && path.is_file() {
            return Some(path);
        }
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let candidate = home.join(".local/bin/codex");
    candidate.is_file().then_some(candidate)
}

pub fn thread_id_from_exec_json(bytes: &[u8]) -> Option<String> {
    for line in bytes.split(|byte| *byte == b'\n') {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let id = value
            .get("thread_id")
            .and_then(Value::as_str)
            .or_else(|| value.pointer("/payload/id").and_then(Value::as_str));
        if value.get("type").and_then(Value::as_str) == Some("thread.started")
            && id.is_some_and(is_uuid)
        {
            return id.map(str::to_owned);
        }
        if value.get("type").and_then(Value::as_str) == Some("session_meta")
            && id.is_some_and(is_uuid)
        {
            return id.map(str::to_owned);
        }
    }
    None
}

fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => *byte == b'-',
        _ => byte.is_ascii_hexdigit(),
    })
}

#[cfg(test)]
mod tests {
    use super::thread_id_from_exec_json;

    #[test]
    fn rejects_blank_and_overlong_names() {
        assert_eq!(super::normalize_task_name("  "), Err("invalid_name"));
        assert_eq!(
            super::normalize_task_name("  晨会  "),
            Ok("晨会".to_owned())
        );
        assert_eq!(
            super::normalize_directory("relative"),
            Err("invalid_directory")
        );
        assert_eq!(
            super::normalize_task_name(&"名".repeat(41)),
            Err("invalid_name")
        );
    }

    #[test]
    fn reads_thread_started_id() {
        let raw = b"{\"type\":\"thread.started\",\"thread_id\":\"019fa972-5cfa-75e1-9008-0b17ade9a347\"}\n";
        assert_eq!(
            thread_id_from_exec_json(raw).as_deref(),
            Some("019fa972-5cfa-75e1-9008-0b17ade9a347")
        );
    }

    #[test]
    fn ignores_non_uuid_and_unrelated_events() {
        let raw = b"{\"type\":\"turn.started\"}\n{\"type\":\"thread.started\",\"thread_id\":\"not-a-uuid\"}\n";
        assert_eq!(thread_id_from_exec_json(raw), None);
    }
}

#[cfg(target_os = "macos")]
mod create_task;
#[cfg(target_os = "macos")]
mod lan_setup;

#[cfg(target_os = "macos")]
use easy_codex_host::health::{
    DashboardSnapshot, HEALTH_SOCKET_NAME, HealthError, HealthSnapshot, PlayFocusCue,
    bind_dashboard_slot, bind_external_slot, query_dashboard, query_health, query_play_focus,
};
use tauri::Manager;
#[cfg(target_os = "macos")]
use easy_codex_host::paths::AppPaths;
#[cfg(target_os = "macos")]
use serde::Serialize;

#[cfg(target_os = "macos")]
#[derive(Debug, Serialize)]
#[serde(tag = "connection", rename_all = "snake_case")]
enum HostProbe {
    Healthy { health: HealthSnapshot },
    Offline { reason: &'static str },
    ProtocolError { reason: &'static str },
}

#[cfg(target_os = "macos")]
#[derive(Debug, Serialize)]
#[serde(tag = "connection", rename_all = "snake_case")]
enum DashboardProbe {
    Healthy { dashboard: DashboardSnapshot },
    Offline { reason: &'static str },
    ProtocolError { reason: &'static str },
}

#[cfg(target_os = "macos")]
fn app_paths() -> Option<AppPaths> {
    std::env::var_os("HOME").map(|home| AppPaths::from_home(std::path::Path::new(&home)))
}

#[cfg(target_os = "macos")]
fn is_offline(error: &HealthError) -> bool {
    matches!(
        error,
        HealthError::Io(source)
            if matches!(
                source.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::TimedOut
            )
    )
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn host_health() -> HostProbe {
    let Some(paths) = app_paths() else {
        return HostProbe::Offline {
            reason: "home_unavailable",
        };
    };
    match query_health(&paths.runtime_directory.join(HEALTH_SOCKET_NAME)) {
        Ok(health) => HostProbe::Healthy { health },
        Err(error) if is_offline(&error) => HostProbe::Offline {
            reason: "host_unreachable",
        },
        Err(_) => HostProbe::ProtocolError {
            reason: "health_invalid",
        },
    }
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn host_play_focus() -> PlayFocusCue {
    let empty = PlayFocusCue {
        v: 1,
        slot: 0,
        seq: 0,
        at_unix_ms: 0,
    };
    let Some(paths) = app_paths() else {
        return empty;
    };
    query_play_focus(&paths.runtime_directory.join(HEALTH_SOCKET_NAME)).unwrap_or(empty)
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn focus_main_window(app: tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn host_dashboard() -> DashboardProbe {
    let Some(paths) = app_paths() else {
        return DashboardProbe::Offline {
            reason: "home_unavailable",
        };
    };
    match query_dashboard(&paths.runtime_directory.join(HEALTH_SOCKET_NAME)) {
        Ok(dashboard) => DashboardProbe::Healthy { dashboard },
        Err(error) if is_offline(&error) => DashboardProbe::Offline {
            reason: "host_unreachable",
        },
        Err(error) => {
            eprintln!("dashboard_probe_failed error={error}");
            DashboardProbe::ProtocolError {
                reason: "dashboard_invalid",
            }
        }
    }
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn bind_slot(
    slot: u8,
    task_id: String,
    expected_generation: Option<u64>,
) -> Result<DashboardSnapshot, &'static str> {
    let Some(paths) = app_paths() else {
        return Err("home_unavailable");
    };
    bind_dashboard_slot(
        &paths.runtime_directory.join(HEALTH_SOCKET_NAME),
        slot,
        &task_id,
        expected_generation,
    )
    .map_err(|error| match error {
        HealthError::Rejected(_) => "binding_rejected",
        error if is_offline(&error) => "host_unreachable",
        _ => "dashboard_invalid",
    })
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn choose_directory() -> Result<String, &'static str> {
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg("POSIX path of (choose folder with prompt \"选择新任务的文件夹\")")
        .output()
        .map_err(|_| "directory_cancelled")?;
    if !output.status.success() {
        return Err("directory_cancelled");
    }
    let path = String::from_utf8_lossy(&output.stdout)
        .trim()
        .trim_end_matches('/')
        .to_owned();
    let path = if path.is_empty() {
        "/".to_owned()
    } else {
        path
    };
    create_task::normalize_directory(&path).map(|path| path.to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn bind_linked_assistant(
    slot: u8,
    provider: String,
    name: String,
    directory: String,
    expected_generation: Option<u64>,
) -> Result<DashboardSnapshot, &'static str> {
    if !(1..=4).contains(&slot) {
        return Err("binding_rejected");
    }
    let Some(paths) = app_paths() else {
        return Err("home_unavailable");
    };
    bind_external_slot(
        &paths.runtime_directory.join(HEALTH_SOCKET_NAME),
        slot,
        &provider,
        &name,
        &directory,
        expected_generation,
    )
    .map_err(|error| match error {
        HealthError::Rejected(reason) if reason == "stale_binding" => "binding_rejected",
        HealthError::Rejected(_) => "binding_rejected",
        error if is_offline(&error) => "host_unreachable",
        _ => "dashboard_invalid",
    })
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn create_and_bind_slot(
    slot: u8,
    name: String,
    directory: String,
    expected_generation: Option<u64>,
) -> Result<DashboardSnapshot, &'static str> {
    if !(1..=4).contains(&slot) {
        return Err("binding_rejected");
    }
    let Some(paths) = app_paths() else {
        return Err("home_unavailable");
    };
    let task_id = create_task::create_thread(&name, &directory)?;
    let socket = paths.runtime_directory.join(HEALTH_SOCKET_NAME);
    let mut rejected = false;
    for _ in 0..8 {
        match bind_dashboard_slot(&socket, slot, &task_id, expected_generation) {
            Ok(dashboard) => return Ok(dashboard),
            Err(HealthError::Rejected(_)) => {
                rejected = true;
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            Err(error) if is_offline(&error) => return Err("host_unreachable"),
            Err(_) => return Err("dashboard_invalid"),
        }
    }
    Err(if rejected {
        "binding_rejected"
    } else {
        "dashboard_invalid"
    })
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn lan_endpoint() -> Result<lan_setup::LanEndpoint, &'static str> {
    lan_setup::lan_endpoint()
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn wifi_choices() -> Result<lan_setup::WifiChoices, &'static str> {
    lan_setup::wifi_choices()
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn save_wifi_profile(ssid: String, password: String) -> Result<(), &'static str> {
    lan_setup::save_wifi_profile(&ssid, &password)
}

#[cfg(target_os = "macos")]
#[tauri::command]
fn forget_wifi_profile(ssid: String) -> Result<(), &'static str> {
    lan_setup::forget_wifi_profile(&ssid)
}

#[cfg(target_os = "macos")]
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDCheckAccess(request_type: i32) -> i32;
    fn IOHIDRequestAccess(request_type: i32) -> bool;
}

#[cfg(target_os = "macos")]
const HID_REQUEST_LISTEN_EVENT: i32 = 1;

/// WKWebView 在主线程收到点击。配网若停在这条线程上，授权窗口弹不出来，键盘回执也进不了 App。
/// 授权检查只在主线程做，而且不能单独拦住写入：开关已经打开时，检查仍可能返回拒绝。
#[cfg(target_os = "macos")]
#[tauri::command]
async fn provision_manual_network(
    app: tauri::AppHandle,
    ssid: String,
    password: String,
) -> Result<(), &'static str> {
    let granted = refresh_listen_access(&app)?;
    let result = tauri::async_runtime::spawn_blocking(move || {
        lan_setup::provision_manual_network(&ssid, &password)
    })
    .await
    .map_err(|_| "provision_failed")?;
    let classified = lan_setup::classify_provision(granted, result);
    if let Err(code) = classified {
        eprintln!("codex-keyboard provision: {code} listen_granted={granted}");
        if code == "keyboard_permission" {
            let _ = std::process::Command::new("open")
                .arg(
                    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ListenEvent",
                )
                .status();
        }
    }
    classified
}

#[cfg(target_os = "macos")]
fn record_listen_access(access: i32) {
    let _ = std::fs::write(
        "/tmp/codex-keyboard-listen.txt",
        format!("access={access}\n"),
    );
}

#[cfg(target_os = "macos")]
fn refresh_listen_access(app: &tauri::AppHandle) -> Result<bool, &'static str> {
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let mut access = unsafe { IOHIDCheckAccess(HID_REQUEST_LISTEN_EVENT) };
        if access != 0 && unsafe { IOHIDRequestAccess(HID_REQUEST_LISTEN_EVENT) } {
            access = 0;
        }
        record_listen_access(access);
        let _ = tx.send(access == 0);
    })
    .map_err(|_| "provision_failed")?;
    rx.recv().map_err(|_| "provision_failed")
}

#[cfg(target_os = "macos")]
fn main() {
    tauri::Builder::default()
        .setup(|_app| {
            record_listen_access(unsafe { IOHIDCheckAccess(HID_REQUEST_LISTEN_EVENT) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            host_health,
            host_dashboard,
            host_play_focus,
            focus_main_window,
            bind_slot,
            choose_directory,
            create_and_bind_slot,
            bind_linked_assistant,
            lan_endpoint,
            wifi_choices,
            save_wifi_profile,
            forget_wifi_profile,
            provision_manual_network
        ])
        .run(tauri::generate_context!())
        .expect("Codex Keyboard desktop runtime failed");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("Codex Keyboard desktop requires macOS");
}

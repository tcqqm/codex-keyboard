use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::process::Command;

use easy_codex_host::lan_voice::LAN_AUDIO_PORT;
use easy_codex_host::paths::{AppPaths, replace_private_file};
use easy_codex_host::provisioning::{
    LanProvisioning, ProvisioningError, load_or_create_device_secret, provision_lan,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const PROFILE_VERSION: u8 = 1;
const MAX_PROFILES: usize = 32;
const CUSTOM_SENTINEL: &str = "@custom";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanEndpoint {
    pub host: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WifiNetwork {
    pub ssid: String,
    pub saved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WifiChoices {
    pub host: String,
    pub networks: Vec<WifiNetwork>,
    pub suggested: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct WifiProfileRecord {
    ssid: String,
    password: String,
}

#[derive(Serialize, Deserialize)]
struct WifiProfileFile {
    v: u8,
    profiles: Vec<WifiProfileRecord>,
}

pub fn lan_endpoint() -> Result<LanEndpoint, &'static str> {
    Ok(LanEndpoint {
        host: lan_host()?.to_string(),
    })
}

pub fn wifi_choices() -> Result<WifiChoices, &'static str> {
    let paths = app_paths()?;
    let profiles = load_profiles(&profiles_path(&paths))?;
    let preferred = preferred_keyboard_names();
    Ok(assemble_choices(lan_host().ok(), &preferred, &profiles))
}

pub fn save_wifi_profile(ssid: &str, password: &str) -> Result<(), &'static str> {
    let paths = app_paths()?;
    save_profiles_at(&paths, ssid, password)
}

pub fn forget_wifi_profile(ssid: &str) -> Result<(), &'static str> {
    let paths = app_paths()?;
    forget_profiles_at(&paths, ssid)
}

pub fn provision_manual_network(ssid: &str, password: &str) -> Result<(), &'static str> {
    let ssid = normalize_ssid(ssid)?;
    let password = if password.is_empty() {
        let paths = app_paths()?;
        let profiles = load_profiles(&profiles_path(&paths))?;
        matching_password(&profiles, &ssid).ok_or("password_unavailable")?
    } else {
        normalize_password(password)?
    };
    let host = lan_host()?;
    let paths = app_paths()?;
    let secret = load_or_create_device_secret(&paths).map_err(|_| "provision_failed")?;
    let config = LanProvisioning::new(ssid, password, IpAddr::V4(host), LAN_AUDIO_PORT, secret)
        .map_err(provision_error_code)?;
    provision_lan(&config)
        .map(|_| ())
        .map_err(provision_error_code)
}

/// 名称和密码都由用户确认。这里不改写手填的名字。
pub fn normalize_wifi(ssid: &str, password: &str) -> Result<(String, String), &'static str> {
    Ok((normalize_ssid(ssid)?, normalize_password(password)?))
}

fn normalize_ssid(ssid: &str) -> Result<String, &'static str> {
    let ssid = ssid.trim();
    if ssid.is_empty() || ssid == CUSTOM_SENTINEL || ssid.len() > 32 || ssid.as_bytes().contains(&0)
    {
        return Err("wifi_unavailable");
    }
    Ok(ssid.to_owned())
}

fn normalize_password(password: &str) -> Result<String, &'static str> {
    if password.is_empty() || password.len() > 64 || password.as_bytes().contains(&0) {
        return Err("password_unavailable");
    }
    Ok(password.to_owned())
}

/// 系统偏好列表里的 5 GHz 名字只用来推出键盘能加入的 2.4 GHz 名字。
fn keyboard_ssid(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed == "<redacted>" {
        return None;
    }
    let name = match trimmed.strip_suffix("-5G") {
        Some(base) if !base.is_empty() => base,
        _ => trimmed,
    };
    if name.is_empty() || name.len() > 32 || name.as_bytes().contains(&0) {
        return None;
    }
    Some(name.to_owned())
}

fn keyboard_names_from_preferred(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if index == 0 && trimmed.starts_with("Preferred networks") {
            continue;
        }
        let Some(name) = keyboard_ssid(trimmed) else {
            continue;
        };
        if seen.insert(name.clone()) {
            names.push(name);
        }
    }
    names
}

/// 2.4 GHz 名称和多一个 `-5G` 的名称是同一组密码。
fn band_names(ssid: &str) -> [String; 2] {
    let base = match ssid.strip_suffix("-5G") {
        Some(base) if !base.is_empty() => base,
        _ => ssid,
    };
    [base.to_owned(), format!("{base}-5G")]
}

fn matching_password(profiles: &[WifiProfileRecord], ssid: &str) -> Option<String> {
    let names = band_names(ssid);
    profiles
        .iter()
        .find(|profile| names.iter().any(|name| name == &profile.ssid))
        .map(|profile| profile.password.clone())
}

fn assemble_choices(
    host: Option<Ipv4Addr>,
    preferred: &[String],
    profiles: &[WifiProfileRecord],
) -> WifiChoices {
    let mut networks = Vec::new();
    let mut seen = HashSet::new();
    for profile in profiles {
        let Some(name) = keyboard_ssid(&profile.ssid) else {
            continue;
        };
        if seen.insert(name.clone()) {
            networks.push(WifiNetwork {
                ssid: name,
                saved: true,
            });
        }
    }
    for name in preferred {
        if seen.insert(name.clone()) {
            networks.push(WifiNetwork {
                ssid: name.clone(),
                saved: false,
            });
        }
    }
    let suggested = networks
        .first()
        .map(|network| network.ssid.clone())
        .unwrap_or_default();
    WifiChoices {
        host: host.map(|address| address.to_string()).unwrap_or_default(),
        networks,
        suggested,
    }
}

fn preferred_keyboard_names() -> Vec<String> {
    let Ok(text) = command_stdout(
        "/usr/sbin/networksetup",
        &["-listpreferredwirelessnetworks", "en0"],
    ) else {
        return Vec::new();
    };
    keyboard_names_from_preferred(&text)
}

fn save_profiles_at(paths: &AppPaths, ssid: &str, password: &str) -> Result<(), &'static str> {
    let (ssid, password) = normalize_wifi(ssid, password)?;
    let mut profiles = load_profiles(&profiles_path(paths))?;
    let names = band_names(&ssid);
    profiles.retain(|profile| !names.iter().any(|name| name == &profile.ssid));
    profiles.insert(0, WifiProfileRecord { ssid, password });
    profiles.truncate(MAX_PROFILES);
    store_profiles(paths, &profiles)
}

fn forget_profiles_at(paths: &AppPaths, ssid: &str) -> Result<(), &'static str> {
    let ssid = normalize_ssid(ssid)?;
    let mut profiles = load_profiles(&profiles_path(paths))?;
    let names = band_names(&ssid);
    let before = profiles.len();
    profiles.retain(|profile| !names.iter().any(|name| name == &profile.ssid));
    if profiles.len() == before {
        return Ok(());
    }
    store_profiles(paths, &profiles)
}

fn app_paths() -> Result<AppPaths, &'static str> {
    let home = std::env::var_os("HOME").ok_or("home_unavailable")?;
    Ok(AppPaths::from_home(Path::new(&home)))
}

fn profiles_path(paths: &AppPaths) -> PathBuf {
    paths.root.join("wifi-profiles.json")
}

fn load_profiles(path: &Path) -> Result<Vec<WifiProfileRecord>, &'static str> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("profile_unavailable"),
    };
    if !metadata.is_file() {
        return Err("profile_unavailable");
    }
    let mut file = File::open(path).map_err(|_| "profile_unavailable")?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.read_to_end(&mut bytes)
        .map_err(|_| "profile_unavailable")?;
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let stored: WifiProfileFile =
        serde_json::from_slice(&bytes).map_err(|_| "profile_unavailable")?;
    if stored.v != PROFILE_VERSION {
        return Err("profile_unavailable");
    }
    for profile in &stored.profiles {
        normalize_wifi(&profile.ssid, &profile.password)?;
    }
    Ok(stored.profiles)
}

fn store_profiles(paths: &AppPaths, profiles: &[WifiProfileRecord]) -> Result<(), &'static str> {
    paths.prepare().map_err(|_| "profile_unavailable")?;
    let path = profiles_path(paths);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => return Err("profile_unavailable"),
    }
    let file = WifiProfileFile {
        v: PROFILE_VERSION,
        profiles: profiles.to_vec(),
    };
    let bytes = Zeroizing::new(serde_json::to_vec(&file).map_err(|_| "profile_unavailable")?);
    replace_private_file(&path, &bytes).map_err(|_| "profile_unavailable")
}

fn lan_host() -> Result<Ipv4Addr, &'static str> {
    let address = command_stdout("/usr/sbin/ipconfig", &["getifaddr", "en0"])?;
    let host: Ipv4Addr = address.trim().parse().map_err(|_| "lan_unavailable")?;
    if !routable_lan(host) {
        return Err("lan_unavailable");
    }
    Ok(host)
}

fn command_stdout(program: &str, args: &[&str]) -> Result<String, &'static str> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|_| "lan_unavailable")?;
    if !output.status.success() {
        return Err("lan_unavailable");
    }
    String::from_utf8(output.stdout).map_err(|_| "lan_unavailable")
}

fn routable_lan(host: Ipv4Addr) -> bool {
    !host.is_unspecified()
        && !host.is_loopback()
        && !host.is_multicast()
        && !host.is_broadcast()
        && !host.is_link_local()
}

/// 授权检查失败时仍会先写 USB。只有写入本身失败，才提示去开输入监控。
pub fn classify_provision(
    listen_granted: bool,
    result: Result<(), &'static str>,
) -> Result<(), &'static str> {
    match result {
        Ok(()) => Ok(()),
        Err(code)
            if !listen_granted
                && matches!(
                    code,
                    "keyboard_open_failed" | "keyboard_write_failed" | "keyboard_no_receipt"
                ) =>
        {
            Err("keyboard_permission")
        }
        Err(code) => Err(code),
    }
}

fn provision_error_code(error: ProvisioningError) -> &'static str {
    match error {
        ProvisioningError::InvalidSsid => "wifi_unavailable",
        ProvisioningError::InvalidPassword => "password_unavailable",
        ProvisioningError::InvalidHost | ProvisioningError::InvalidPort => "lan_unavailable",
        ProvisioningError::DeviceNotFound => "keyboard_unplugged",
        ProvisioningError::HidOpen => "keyboard_open_failed",
        ProvisioningError::HidWrite => "keyboard_write_failed",
        ProvisioningError::Hid => "keyboard_no_receipt",
        ProvisioningError::HidAck => "keyboard_rejected",
        ProvisioningError::InvalidPayload | ProvisioningError::UnsupportedPlatform => {
            "provision_failed"
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::{
        assemble_choices, classify_provision, keyboard_names_from_preferred, load_profiles,
        normalize_wifi, profiles_path, provision_error_code, save_profiles_at,
    };
    use easy_codex_host::paths::AppPaths;
    use easy_codex_host::provisioning::ProvisioningError;

    #[test]
    fn missing_usb_cable_is_not_the_same_as_a_rejected_write() {
        assert_eq!(
            provision_error_code(ProvisioningError::DeviceNotFound),
            "keyboard_unplugged"
        );
        assert_eq!(
            provision_error_code(ProvisioningError::HidAck),
            "keyboard_rejected"
        );
        assert_eq!(
            provision_error_code(ProvisioningError::Hid),
            "keyboard_no_receipt"
        );
        assert_eq!(
            provision_error_code(ProvisioningError::HidOpen),
            "keyboard_open_failed"
        );
        assert_eq!(
            provision_error_code(ProvisioningError::HidWrite),
            "keyboard_write_failed"
        );
    }

    #[test]
    fn denied_listen_access_still_keeps_a_successful_write() {
        assert_eq!(classify_provision(false, Ok(())), Ok(()));
        assert_eq!(
            classify_provision(false, Err("keyboard_no_receipt")),
            Err("keyboard_permission")
        );
        assert_eq!(
            classify_provision(true, Err("keyboard_no_receipt")),
            Err("keyboard_no_receipt")
        );
        assert_eq!(
            classify_provision(false, Err("keyboard_unplugged")),
            Err("keyboard_unplugged")
        );
    }

    #[test]
    fn manual_wifi_keeps_the_typed_name_and_rejects_blanks() {
        assert_eq!(
            normalize_wifi("  lab-24  ", "secret"),
            Ok(("lab-24".to_owned(), "secret".to_owned()))
        );
        assert_eq!(normalize_wifi("", "secret"), Err("wifi_unavailable"));
        assert_eq!(normalize_wifi("lab-24", ""), Err("password_unavailable"));
        assert_eq!(
            normalize_wifi("lab-24-5G", "secret"),
            Ok(("lab-24-5G".to_owned(), "secret".to_owned()))
        );
    }

    #[test]
    fn wifi_choices_offer_saved_names_and_drop_the_5g_suffix() {
        let preferred = keyboard_names_from_preferred(
            "Preferred networks on en0:\n\tlab-24-5G\n\tlab-24\n\tcafe\n\t<redacted>\n\t\n\tother-5G\n",
        );
        assert_eq!(preferred, vec!["lab-24", "cafe", "other"]);
        let saved = [super::WifiProfileRecord {
            ssid: "cafe".to_owned(),
            password: "secret-value".to_owned(),
        }];
        let choices = assemble_choices(None, &preferred, &saved);
        assert_eq!(choices.suggested, "cafe");
        assert_eq!(
            choices
                .networks
                .iter()
                .map(|network| (network.ssid.as_str(), network.saved))
                .collect::<Vec<_>>(),
            vec![("cafe", true), ("lab-24", false), ("other", false)]
        );
        let json = serde_json::to_string(&choices).unwrap();
        assert!(!json.contains("secret-value"));
    }

    #[test]
    fn five_g_name_matches_the_saved_password_on_the_24_name() {
        let saved = [super::WifiProfileRecord {
            ssid: "lab-24-5G".to_owned(),
            password: "secret-value".to_owned(),
        }];
        assert_eq!(
            super::matching_password(&saved, "lab-24").as_deref(),
            Some("secret-value")
        );
        assert_eq!(
            super::matching_password(&saved, "lab-24-5G").as_deref(),
            Some("secret-value")
        );
        let preferred =
            keyboard_names_from_preferred("Preferred networks on en0:\n\tlab-24-5G\n\tcafe\n");
        let choices = assemble_choices(None, &preferred, &saved);
        assert_eq!(
            choices
                .networks
                .iter()
                .map(|network| (network.ssid.as_str(), network.saved))
                .collect::<Vec<_>>(),
            vec![("lab-24", true), ("cafe", false)]
        );
        let json = serde_json::to_string(&choices).unwrap();
        assert!(!json.contains("secret-value"));
        assert!(!json.contains("lab-24-5G"));
    }

    #[test]
    fn wifi_profile_roundtrip_is_private_and_replaces_the_same_name() {
        let temp = std::env::temp_dir().join(format!(
            "codex-keyboard-wifi-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = AppPaths::from_root(temp.join("app-support"));
        let saved = save_profiles_at(&paths, "lab-24", "secret-value");
        assert_eq!(saved, Ok(()));
        let path = profiles_path(&paths);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let loaded = load_profiles(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].ssid, "lab-24");
        assert_eq!(loaded[0].password, "secret-value");
        save_profiles_at(&paths, "lab-24", "next-secret").unwrap();
        let replaced = load_profiles(&path).unwrap();
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0].password, "next-secret");
        save_profiles_at(&paths, "lab-24-5G", "secret-value").unwrap();
        save_profiles_at(&paths, "lab-24", "next-secret").unwrap();
        let paired = load_profiles(&path).unwrap();
        assert_eq!(paired.len(), 1);
        assert_eq!(paired[0].ssid, "lab-24");
        assert_eq!(
            super::matching_password(&paired, "lab-24-5G").as_deref(),
            Some("next-secret")
        );
        super::forget_profiles_at(&paths, "lab-24-5G").unwrap();
        assert!(load_profiles(&path).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&temp);
    }
}

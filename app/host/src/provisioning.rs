#[cfg(any(target_os = "macos", test))]
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
#[cfg(target_os = "macos")]
use std::thread;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

use serde::Serialize;
use thiserror::Error;
use zeroize::Zeroize;

#[cfg(test)]
use crate::lan_voice::LAN_AUDIO_PORT;
use crate::paths::{AppPaths, open_private_file, replace_private_file};

const CONFIG_REPORT_ID: u8 = 0x10;
const REPORT_BYTES: usize = 64;
const REPORT_BODY_BYTES: usize = REPORT_BYTES - 1;
const CONFIG_HEADER_BYTES: usize = 11;
const CONFIG_CHUNK_BYTES: usize = REPORT_BODY_BYTES - CONFIG_HEADER_BYTES;
const MAX_CONFIG_BYTES: usize = 2048;
#[cfg(any(target_os = "macos", test))]
const APP_COMMAND_REPORT_ID: u8 = 0x11;
#[cfg(any(target_os = "macos", test))]
const APP_COMMAND_CONFIG_ACK: u8 = 0x03;
#[cfg(target_os = "macos")]
const CONFIG_ACK_TIMEOUT: Duration = Duration::from_secs(4);
#[cfg(any(target_os = "macos", test))]
const EASY_INPUT_USB_VID: u16 = 0x303A;
#[cfg(any(target_os = "macos", test))]
const EASY_INPUT_USB_PID: u16 = 0x1006;
#[cfg(any(target_os = "macos", test))]
const CONFIG_USAGE_PAGE: u16 = 0xFF00;
#[cfg(any(target_os = "macos", test))]
const CONFIG_USAGE: u16 = 0x0002;

#[derive(Debug, Error)]
pub enum ProvisioningError {
    #[error("Wi-Fi SSID must be 1..=32 bytes")]
    InvalidSsid,
    #[error("Wi-Fi password exceeds 64 bytes")]
    InvalidPassword,
    #[error("audio host must be a routable IPv4 address")]
    InvalidHost,
    #[error("audio port must be in 1024..=65535")]
    InvalidPort,
    #[error("provisioning payload is invalid")]
    InvalidPayload,
    #[error("AI keyboard HID device was not found")]
    DeviceNotFound,
    #[error("AI keyboard HID provisioning failed")]
    Hid,
    #[error("AI keyboard HID device could not be opened")]
    HidOpen,
    #[error("AI keyboard HID feature report write failed")]
    HidWrite,
    #[error("AI keyboard did not return the exact saved configuration ACK")]
    HidAck,
    #[error("HID provisioning is currently supported only on macOS")]
    UnsupportedPlatform,
}

#[derive(Clone, PartialEq, Eq)]
pub struct LanProvisioning {
    ssid: String,
    password: String,
    host: Ipv4Addr,
    port: u16,
    device_secret: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisioningReceipt {
    pub transport: &'static str,
    pub product: String,
    pub payload_bytes: usize,
    pub chunks: usize,
    pub crc16: u16,
}

#[derive(Serialize)]
struct ProvisionPayload<'a> {
    schema: &'static str,
    device_name: &'static str,
    wifi_ssid: &'a str,
    wifi_password: &'a str,
    audio_host: String,
    audio_port: u16,
    audio_enabled: bool,
    speaker_sync_key: String,
    speaker_sync_key_epoch: u16,
    profiles: Vec<Profile>,
}

#[derive(Serialize)]
struct Profile {
    id: &'static str,
    keys: std::collections::BTreeMap<&'static str, Key>,
    encoder: Encoder,
}

#[derive(Serialize)]
struct Key {
    press: &'static str,
}

#[derive(Serialize)]
struct Encoder {
    left: &'static str,
    right: &'static str,
    press: &'static str,
}

impl LanProvisioning {
    pub fn new(
        ssid: String,
        password: String,
        host: IpAddr,
        port: u16,
        device_secret: [u8; 32],
    ) -> Result<Self, ProvisioningError> {
        if ssid.is_empty() || ssid.len() > 32 {
            return Err(ProvisioningError::InvalidSsid);
        }
        if password.len() > 64 {
            return Err(ProvisioningError::InvalidPassword);
        }
        let IpAddr::V4(host) = host else {
            return Err(ProvisioningError::InvalidHost);
        };
        if host.is_unspecified()
            || host.is_loopback()
            || host.is_multicast()
            || host.is_broadcast()
            || host.is_link_local()
        {
            return Err(ProvisioningError::InvalidHost);
        }
        if port < 1024 {
            return Err(ProvisioningError::InvalidPort);
        }
        if device_secret.iter().all(|byte| *byte == 0) {
            return Err(ProvisioningError::InvalidPayload);
        }
        Ok(Self {
            ssid,
            password,
            host,
            port,
            device_secret,
        })
    }

    pub fn payload_json(&self) -> Result<Vec<u8>, ProvisioningError> {
        let keys = (1..=8)
            .map(|slot| {
                let name = match slot {
                    1 => "KEY1",
                    2 => "KEY2",
                    3 => "KEY3",
                    4 => "KEY4",
                    5 => "KEY5",
                    6 => "KEY6",
                    7 => "KEY7",
                    _ => "KEY8",
                };
                (name, Key { press: "disabled" })
            })
            .collect();
        let payload = ProvisionPayload {
            schema: "ai_keyboard.v1",
            device_name: "Easy Codex Input",
            wifi_ssid: &self.ssid,
            wifi_password: &self.password,
            audio_host: self.host.to_string(),
            audio_port: self.port,
            audio_enabled: true,
            speaker_sync_key: hex_encode(&self.device_secret),
            speaker_sync_key_epoch: 1,
            profiles: vec![Profile {
                id: "default",
                keys,
                encoder: Encoder {
                    left: "disabled",
                    right: "disabled",
                    press: "disabled",
                },
            }],
        };
        let json = serde_json::to_vec(&payload).map_err(|_| ProvisioningError::InvalidPayload)?;
        if json.is_empty() || json.len() > MAX_CONFIG_BYTES {
            return Err(ProvisioningError::InvalidPayload);
        }
        Ok(json)
    }

    pub fn reports(&self) -> Result<Vec<[u8; REPORT_BYTES]>, ProvisioningError> {
        encode_reports(&self.payload_json()?)
    }
}

pub fn load_device_secret(paths: &AppPaths) -> Result<[u8; 32], ProvisioningError> {
    load_device_secret_path(&paths.device_secret)
}

pub(crate) fn load_device_secret_path(path: &Path) -> Result<[u8; 32], ProvisioningError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ProvisioningError::InvalidPayload)?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() != 65
    {
        return Err(ProvisioningError::InvalidPayload);
    }
    let mut file = open_private_file(path).map_err(|_| ProvisioningError::InvalidPayload)?;
    let mut encoded = String::with_capacity(65);
    file.read_to_string(&mut encoded)
        .map_err(|_| ProvisioningError::InvalidPayload)?;
    let secret = decode_secret_hex(encoded.trim_end())?;
    if secret.iter().all(|byte| *byte == 0) {
        return Err(ProvisioningError::InvalidPayload);
    }
    Ok(secret)
}

pub fn load_or_create_device_secret(paths: &AppPaths) -> Result<[u8; 32], ProvisioningError> {
    match fs::symlink_metadata(&paths.device_secret) {
        Ok(_) => return load_device_secret(paths),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(ProvisioningError::InvalidPayload),
    }
    paths
        .prepare()
        .map_err(|_| ProvisioningError::InvalidPayload)?;
    let mut random =
        fs::File::open("/dev/urandom").map_err(|_| ProvisioningError::InvalidPayload)?;
    let mut secret = [0_u8; 32];
    random
        .read_exact(&mut secret)
        .map_err(|_| ProvisioningError::InvalidPayload)?;
    if secret.iter().all(|byte| *byte == 0) {
        return Err(ProvisioningError::InvalidPayload);
    }
    let mut encoded = hex_encode(&secret);
    encoded.push('\n');
    replace_private_file(&paths.device_secret, encoded.as_bytes())
        .map_err(|_| ProvisioningError::InvalidPayload)?;
    encoded.zeroize();
    load_device_secret(paths)
}

pub fn provision_lan(config: &LanProvisioning) -> Result<ProvisioningReceipt, ProvisioningError> {
    #[cfg(target_os = "macos")]
    {
        provision_lan_macos(config)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
        Err(ProvisioningError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "macos")]
fn provision_lan_macos(config: &LanProvisioning) -> Result<ProvisioningReceipt, ProvisioningError> {
    let payload = config.payload_json()?;
    let reports = encode_reports(&payload)?;
    let api = hidapi::HidApi::new().map_err(|_| ProvisioningError::Hid)?;
    api.set_open_exclusive(false);
    let devices = api.device_list().cloned().collect::<Vec<_>>();
    let listed = devices.iter().map(listed_hid).collect::<Vec<_>>();
    let indexes = usb_config_channel_indexes(&listed);
    if indexes.is_empty() {
        return Err(ProvisioningError::DeviceNotFound);
    }
    let mut opened = false;
    let mut wrote_all = false;
    let mut last_error = ProvisioningError::HidAck;
    for index in indexes {
        let candidate = &devices[index];
        let Ok(device) = candidate.open_device(&api) else {
            if !opened {
                last_error = ProvisioningError::HidOpen;
            }
            continue;
        };
        opened = true;
        drain_pending_input(&device);
        let mut complete = true;
        for report in &reports {
            if device.send_feature_report(report).is_err() {
                complete = false;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if !complete {
            last_error = ProvisioningError::HidWrite;
            continue;
        }
        wrote_all = true;
        let crc16 = crc16_ccitt(&payload);
        match wait_for_config_ack(&device, payload.len(), crc16) {
            Ok(()) => {
                return Ok(ProvisioningReceipt {
                    transport: "usb_hid",
                    product: candidate
                        .product_string()
                        .unwrap_or("AI Keyboard")
                        .to_owned(),
                    payload_bytes: payload.len(),
                    chunks: reports.len(),
                    crc16,
                });
            }
            Err(error) => last_error = error,
        }
    }
    if !opened {
        Err(ProvisioningError::HidOpen)
    } else if !wrote_all {
        Err(ProvisioningError::HidWrite)
    } else {
        Err(last_error)
    }
}

#[cfg(any(target_os = "macos", test))]
struct ListedHid {
    vendor_id: u16,
    product_id: u16,
    usb: bool,
    usage_page: u16,
    usage: u16,
    path: String,
}

#[cfg(target_os = "macos")]
fn listed_hid(device: &hidapi::DeviceInfo) -> ListedHid {
    ListedHid {
        vendor_id: device.vendor_id(),
        product_id: device.product_id(),
        usb: matches!(device.bus_type(), hidapi::BusType::Usb),
        usage_page: device.usage_page(),
        usage: device.usage(),
        path: device.path().to_string_lossy().into_owned(),
    }
}

/// 同一 VID/PID 在 macOS 上会拆成按键、鼠标和蓝牙节点。配网只打开 USB 厂商配置集合。
#[cfg(any(target_os = "macos", test))]
fn is_usb_config_channel(device: &ListedHid) -> bool {
    device.vendor_id == EASY_INPUT_USB_VID
        && device.product_id == EASY_INPUT_USB_PID
        && device.usb
        && device.usage_page == CONFIG_USAGE_PAGE
        && device.usage == CONFIG_USAGE
}

#[cfg(any(target_os = "macos", test))]
fn usb_config_channel_indexes(devices: &[ListedHid]) -> Vec<usize> {
    let mut seen = HashSet::new();
    let mut indexes = Vec::new();
    for (index, device) in devices.iter().enumerate() {
        if is_usb_config_channel(device) && seen.insert(device.path.clone()) {
            indexes.push(index);
        }
    }
    indexes
}

#[cfg(target_os = "macos")]
fn drain_pending_input(device: &hidapi::HidDevice) {
    let mut report = [0_u8; REPORT_BYTES];
    for _ in 0..32 {
        match device.read_timeout(&mut report, 0) {
            Ok(0) | Err(_) => break,
            Ok(_) => continue,
        }
    }
}

#[cfg(target_os = "macos")]
fn wait_for_config_ack(
    device: &hidapi::HidDevice,
    expected_bytes: usize,
    expected_crc16: u16,
) -> Result<(), ProvisioningError> {
    let expected_bytes = u16::try_from(expected_bytes).map_err(|_| ProvisioningError::Hid)?;
    let deadline = Instant::now() + CONFIG_ACK_TIMEOUT;
    let mut report = [0_u8; REPORT_BYTES];
    let mut saw_other_config_ack = false;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let wait_ms = remaining.as_millis().clamp(1, 200) as i32;
        match device.read_timeout(&mut report, wait_ms) {
            Ok(0) => continue,
            Ok(length) => {
                if note_config_ack(
                    decode_config_ack(&report[..length]),
                    expected_bytes,
                    expected_crc16,
                    &mut saw_other_config_ack,
                ) {
                    return Ok(());
                }
            }
            Err(_) => return Err(ProvisioningError::Hid),
        }
    }
    Err(config_ack_timeout_error(saw_other_config_ack))
}

/// 旧的未保存回执可能还在输入队列里。它不能立刻判失败，要继续等这一次的已保存回执。
#[cfg(any(target_os = "macos", test))]
fn note_config_ack(
    ack: Option<ConfigAck>,
    expected_bytes: u16,
    expected_crc16: u16,
    saw_other_config_ack: &mut bool,
) -> bool {
    let Some(ack) = ack else {
        return false;
    };
    let matched = ack.phase == 1
        && ack.ok
        && ack.saved
        && ack.bytes == expected_bytes
        && ack.crc16 == expected_crc16;
    if !matched {
        *saw_other_config_ack = true;
    }
    matched
}

#[cfg(any(target_os = "macos", test))]
fn config_ack_timeout_error(saw_other_config_ack: bool) -> ProvisioningError {
    if saw_other_config_ack {
        ProvisioningError::HidAck
    } else {
        ProvisioningError::Hid
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConfigAck {
    phase: u8,
    ok: bool,
    bytes: u16,
    crc16: u16,
    saved: bool,
}

#[cfg(any(target_os = "macos", test))]
fn decode_config_ack(report: &[u8]) -> Option<ConfigAck> {
    if report.len() < 12
        || report[0] != APP_COMMAND_REPORT_ID
        || report[1] != APP_COMMAND_CONFIG_ACK
        || report[2] != 0
        || report[3] != 1
        || report[4] != 7
        || report[6] > 1
        || report[11] > 1
    {
        return None;
    }
    Some(ConfigAck {
        phase: report[5],
        ok: report[6] == 1,
        bytes: u16::from_le_bytes([report[7], report[8]]),
        crc16: u16::from_le_bytes([report[9], report[10]]),
        saved: report[11] == 1,
    })
}

fn encode_reports(json: &[u8]) -> Result<Vec<[u8; REPORT_BYTES]>, ProvisioningError> {
    if json.is_empty() || json.len() > MAX_CONFIG_BYTES {
        return Err(ProvisioningError::InvalidPayload);
    }
    let chunks = json.chunks(CONFIG_CHUNK_BYTES).collect::<Vec<_>>();
    if chunks.is_empty() || chunks.len() > u8::MAX as usize || json.len() > u16::MAX as usize {
        return Err(ProvisioningError::InvalidPayload);
    }
    let total_bytes = json.len() as u16;
    let crc = crc16_ccitt(json);
    let total_chunks = chunks.len() as u8;
    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let mut report = [0_u8; REPORT_BYTES];
            report[0] = CONFIG_REPORT_ID;
            report[1..4].copy_from_slice(b"S3C");
            report[4] = 1;
            report[5] = index as u8;
            report[6] = total_chunks;
            report[7..9].copy_from_slice(&total_bytes.to_le_bytes());
            report[9] = chunk.len() as u8;
            report[10..12].copy_from_slice(&crc.to_le_bytes());
            report[12..12 + chunk.len()].copy_from_slice(chunk);
            report
        })
        .collect())
}

fn crc16_ccitt(bytes: &[u8]) -> u16 {
    let mut crc = 0xFFFF_u16;
    for byte in bytes {
        crc ^= (*byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_secret_hex(value: &str) -> Result<[u8; 32], ProvisioningError> {
    if value.len() != 64 {
        return Err(ProvisioningError::InvalidPayload);
    }
    let mut secret = [0_u8; 32];
    for (index, byte) in secret.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| ProvisioningError::InvalidPayload)?;
    }
    if secret.iter().all(|byte| *byte == 0) {
        return Err(ProvisioningError::InvalidPayload);
    }
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    #[test]
    fn payload_enables_lan_audio_without_exposing_password_in_receipt() {
        let config = LanProvisioning::new(
            "Office WiFi".to_owned(),
            "secret password".to_owned(),
            "192.168.1.20".parse().unwrap(),
            LAN_AUDIO_PORT,
            [7; 32],
        )
        .unwrap();
        let payload = config.payload_json().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(value["schema"], "ai_keyboard.v1");
        assert_eq!(value["audio_enabled"], true);
        assert_eq!(value["wifi_ssid"], "Office WiFi");
        assert_eq!(value["wifi_password"], "secret password");
        assert_eq!(value["audio_host"], "192.168.1.20");
        assert_eq!(
            value["speaker_sync_key"],
            "0707070707070707070707070707070707070707070707070707070707070707"
        );
        assert_eq!(value["speaker_sync_key_epoch"], 1);
        assert_eq!(value["profiles"][0]["keys"]["KEY1"]["press"], "disabled");
    }

    #[test]
    fn reports_match_s3c_wire_and_reassemble_exact_payload() {
        let config = LanProvisioning::new(
            "Office WiFi".to_owned(),
            "password".to_owned(),
            "192.168.1.20".parse().unwrap(),
            LAN_AUDIO_PORT,
            [8; 32],
        )
        .unwrap();
        let payload = config.payload_json().unwrap();
        let reports = config.reports().unwrap();
        assert!(reports.len() > 1);
        let mut reassembled = Vec::new();
        for (index, report) in reports.iter().enumerate() {
            assert_eq!(report[0], CONFIG_REPORT_ID);
            assert_eq!(&report[1..4], b"S3C");
            assert_eq!(report[4], 1);
            assert_eq!(report[5], index as u8);
            assert_eq!(report[6], reports.len() as u8);
            assert_eq!(
                u16::from_le_bytes([report[7], report[8]]) as usize,
                payload.len()
            );
            assert_eq!(
                u16::from_le_bytes([report[10], report[11]]),
                crc16_ccitt(&payload)
            );
            let length = report[9] as usize;
            reassembled.extend_from_slice(&report[12..12 + length]);
        }
        assert_eq!(reassembled, payload);
    }

    #[test]
    fn config_ack_requires_exact_saved_fingerprint() {
        let mut report = [0_u8; REPORT_BYTES];
        report[0] = APP_COMMAND_REPORT_ID;
        report[1] = APP_COMMAND_CONFIG_ACK;
        report[3] = 1;
        report[4] = 7;
        report[5] = 1;
        report[6] = 1;
        report[7..9].copy_from_slice(&777_u16.to_le_bytes());
        report[9..11].copy_from_slice(&0xBEEF_u16.to_le_bytes());
        report[11] = 1;
        assert_eq!(
            decode_config_ack(&report),
            Some(ConfigAck {
                phase: 1,
                ok: true,
                bytes: 777,
                crc16: 0xBEEF,
                saved: true,
            })
        );
        report[11] = 0;
        assert!(!decode_config_ack(&report).unwrap().saved);
        report[4] = 6;
        assert!(decode_config_ack(&report).is_none());
    }

    #[test]
    fn invalid_network_values_fail_before_hid_access() {
        assert!(
            LanProvisioning::new(
                "".into(),
                "".into(),
                "192.168.1.2".parse().unwrap(),
                17333,
                [1; 32],
            )
            .is_err()
        );
        assert!(
            LanProvisioning::new(
                "wifi".into(),
                "".into(),
                "127.0.0.1".parse().unwrap(),
                17333,
                [1; 32],
            )
            .is_err()
        );
        assert!(
            LanProvisioning::new(
                "wifi".into(),
                "".into(),
                "192.168.1.2".parse().unwrap(),
                80,
                [1; 32],
            )
            .is_err()
        );
        assert!(
            LanProvisioning::new(
                "wifi".into(),
                "x".repeat(65),
                "192.168.1.2".parse().unwrap(),
                17333,
                [1; 32],
            )
            .is_err()
        );
        assert!(
            LanProvisioning::new(
                "wifi".into(),
                "".into(),
                "192.168.1.2".parse().unwrap(),
                17333,
                [0; 32],
            )
            .is_err()
        );
    }

    fn listed(usb: bool, usage_page: u16, usage: u16, path: &str) -> ListedHid {
        ListedHid {
            vendor_id: 0x303A,
            product_id: 0x1006,
            usb,
            usage_page,
            usage,
            path: path.to_owned(),
        }
    }

    fn config_ack_report(saved: bool, bytes: u16, crc16: u16) -> [u8; REPORT_BYTES] {
        let mut report = [0_u8; REPORT_BYTES];
        report[0] = APP_COMMAND_REPORT_ID;
        report[1] = APP_COMMAND_CONFIG_ACK;
        report[3] = 1;
        report[4] = 7;
        report[5] = 1;
        report[6] = u8::from(saved);
        report[7..9].copy_from_slice(&bytes.to_le_bytes());
        report[9..11].copy_from_slice(&crc16.to_le_bytes());
        report[11] = u8::from(saved);
        report
    }

    #[test]
    fn config_channel_ignores_keyboard_mouse_and_bluetooth() {
        let devices = vec![
            listed(true, 0x0001, 0x0006, "usb"),
            listed(true, 0x0001, 0x0002, "usb"),
            listed(true, 0x0001, 0x0001, "usb"),
            listed(true, CONFIG_USAGE_PAGE, CONFIG_USAGE, "usb"),
            listed(false, CONFIG_USAGE_PAGE, CONFIG_USAGE, "ble"),
            listed(false, 0x0001, 0x0006, "ble"),
            ListedHid {
                vendor_id: 0xFFFF,
                product_id: 0x1006,
                usb: true,
                usage_page: CONFIG_USAGE_PAGE,
                usage: CONFIG_USAGE,
                path: "other".to_owned(),
            },
        ];
        assert_eq!(usb_config_channel_indexes(&devices), vec![3]);
    }

    #[test]
    fn config_channel_opens_a_duplicate_path_once() {
        let devices = vec![
            listed(true, CONFIG_USAGE_PAGE, CONFIG_USAGE, "usb"),
            listed(true, CONFIG_USAGE_PAGE, CONFIG_USAGE, "usb"),
        ];
        assert_eq!(usb_config_channel_indexes(&devices), vec![0]);
    }

    #[test]
    fn config_channel_treats_bluetooth_alone_as_missing() {
        let devices = vec![listed(false, CONFIG_USAGE_PAGE, CONFIG_USAGE, "ble")];
        assert!(usb_config_channel_indexes(&devices).is_empty());
    }

    #[test]
    fn config_channel_keeps_waiting_after_a_stale_ack() {
        let mut saw_other = false;
        let stale = decode_config_ack(&config_ack_report(false, 17, 0xCAFF));
        assert!(!note_config_ack(stale, 800, 0xBEEF, &mut saw_other));
        assert!(saw_other);
        let saved = decode_config_ack(&config_ack_report(true, 800, 0xBEEF));
        assert!(note_config_ack(saved, 800, 0xBEEF, &mut saw_other));
        assert!(matches!(
            config_ack_timeout_error(true),
            ProvisioningError::HidAck
        ));
        assert!(matches!(
            config_ack_timeout_error(false),
            ProvisioningError::Hid
        ));
    }

    #[test]
    fn config_channel_ignores_reports_that_are_not_acks() {
        let mut saw_other = false;
        assert!(!note_config_ack(None, 10, 1, &mut saw_other));
        assert!(!saw_other);
    }

    #[test]
    fn device_secret_is_private_persistent_and_nonzero() {
        let temp = tempdir().unwrap();
        let paths = AppPaths::from_root(temp.path().join("app-support"));
        let first = load_or_create_device_secret(&paths).unwrap();
        let second = load_or_create_device_secret(&paths).unwrap();
        assert_eq!(first, second);
        assert!(first.iter().any(|byte| *byte != 0));
        let metadata = fs::symlink_metadata(&paths.device_secret).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(metadata.len(), 65);
    }

    #[test]
    fn existing_symlink_secret_is_rejected_without_replacement() {
        let temp = tempdir().unwrap();
        let paths = AppPaths::from_root(temp.path().join("app-support"));
        paths.prepare().unwrap();
        let target = temp.path().join("target");
        fs::write(&target, b"do-not-touch").unwrap();
        symlink(&target, &paths.device_secret).unwrap();
        assert!(load_or_create_device_secret(&paths).is_err());
        assert!(
            fs::symlink_metadata(&paths.device_secret)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&target).unwrap(), b"do-not-touch");
    }
}

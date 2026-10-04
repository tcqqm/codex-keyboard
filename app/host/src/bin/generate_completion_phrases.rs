use std::fs;
use std::path::PathBuf;

use easy_codex_host::audio::{encode_tts_audio, transcode_eiad_for_device};
use easy_codex_host::dashscope::{DashScopeTtsClient, TTS_MODEL, TtsRequest};
use easy_codex_host::paths::AppPaths;
use easy_codex_host::secrets::{DashScopeEnvStore, ImportLock, KeychainAccounts};
use easy_codex_host::summary_orchestrator::{
    SUMMARY_TTS_INSTRUCTIONS, SUMMARY_TTS_VOICE, completion_phrase,
};
use sha2::{Digest, Sha256};

/// 把四句完成播报做成板载 EIAD 和 Host 用的 PCM。只在准备固件资源时跑一次。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: generate_completion_phrases <output-directory>")?;
    fs::create_dir_all(&output)?;

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let paths = AppPaths::from_home(&home);
    paths.prepare()?;
    let lock = ImportLock::acquire(&paths.runtime_directory.join("key-import.lock"))?;
    let accounts = KeychainAccounts::load_or_create(&paths.installation_id, &lock)?;
    let secrets = DashScopeEnvStore::new(paths.dashscope_env, &accounts);
    let client = DashScopeTtsClient::default();

    let phrases = [
        ("slot0", completion_phrase(None)),
        ("slot1", completion_phrase(Some(1))),
        ("slot2", completion_phrase(Some(2))),
        ("slot3", completion_phrase(Some(3))),
        ("slot4", completion_phrase(Some(4))),
    ];
    for (name, text) in phrases {
        eprintln!("generating name={name} characters={}", text.chars().count());
        let audio = client.synthesize(
            &secrets,
            &accounts,
            TtsRequest {
                text,
                voice: SUMMARY_TTS_VOICE,
                instructions: SUMMARY_TTS_INSTRUCTIONS,
            },
        )?;
        let pcm = audio.pcm();
        let host = encode_tts_audio(pcm)?;
        let device = transcode_eiad_for_device(host.eiad())?;
        let pcm_path = output.join(format!("{name}.pcm"));
        let eiad_path = output.join(format!("{name}.eiad"));
        if pcm_path.exists() || eiad_path.exists() {
            return Err(format!("refusing to overwrite {name}").into());
        }
        fs::write(&pcm_path, pcm)?;
        fs::write(&eiad_path, device.as_slice())?;
        let digest = Sha256::digest(device.as_slice());
        eprintln!(
            "wrote name={name} model={TTS_MODEL} voice={SUMMARY_TTS_VOICE} samples={} eiad_bytes={} eiad_sha256={}",
            pcm.len() / 2,
            device.len(),
            digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
    }
    Ok(())
}

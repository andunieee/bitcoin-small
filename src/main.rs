// desktop build, and the web build when compiled to wasm

#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    wasm_bindgen_futures::spawn_local(async {
        if let Err(err) = bitcoin_smol::run().await {
            log::error!("app crashed: {err}");
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), slint::PlatformError> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let data_dir = std::env::var_os("BITCOIN_SMOL_DATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(default_data_dir);
    bitcoin_smol::run(data_dir)
}

#[cfg(windows)]
fn default_data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("bitcoin-smol")
}

#[cfg(all(not(windows), not(target_arch = "wasm32")))]
fn default_data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join(".local/share/bitcoin-smol")
}

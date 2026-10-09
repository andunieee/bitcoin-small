// desktop build, for development

fn main() -> Result<(), slint::PlatformError> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let data_dir = std::env::var_os("BITCOIN_SMALL_DATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(default_data_dir);
    bitcoin_small::run(data_dir)
}

#[cfg(windows)]
fn default_data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("bitcoin-small")
}

#[cfg(not(windows))]
fn default_data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join(".local/share/bitcoin-small")
}

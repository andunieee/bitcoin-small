// desktop build, for development

fn main() -> Result<(), slint::PlatformError> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let data_dir = std::env::var_os("BITCOIN_SMALL_DATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".local/share/bitcoin-small")
        });
    bitcoin_small::run(data_dir)
}

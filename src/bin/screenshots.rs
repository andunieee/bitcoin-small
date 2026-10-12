// renders the app's screens over made-up data, into the directory given (docs/static/screenshots by default)

fn main() -> Result<(), slint::PlatformError> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let out = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "docs/static/screenshots".into());
    bitcoin_smol::screenshots(&out)
}

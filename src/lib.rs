mod app;
pub mod form;
pub mod prices;
pub mod protocol;
pub mod store;

slint::include_modules!();

pub use app::run;

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(android_app: slint::android::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("bitcoin-small"),
    );

    // the websocket tls stack looks for the system's ca certificates through this
    // SAFETY: nothing else is running yet
    unsafe { std::env::set_var("SSL_CERT_DIR", "/system/etc/security/cacerts") };

    let data_dir = android_app
        .internal_data_path()
        .expect("android gives every app an internal data dir");
    slint::android::init(android_app).expect("failed to init slint on android");
    run(data_dir).expect("app crashed");
}

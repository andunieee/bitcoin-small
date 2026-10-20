pub mod address;
mod app;
pub mod deal;
pub mod xfk;
pub mod form;
pub mod history;
mod notify;
pub mod prices;
pub mod protocol;
pub mod relays;
pub mod store;

slint::include_modules!();

pub use app::run;
#[cfg(feature = "screenshots")]
pub use app::screenshots::run as screenshots;

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(android_app: slint::android::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("bitcoin-smol"),
    );

    // the websocket tls stack looks for the system's ca certificates through this
    // SAFETY: nothing else is running yet
    unsafe { std::env::set_var("SSL_CERT_DIR", "/system/etc/security/cacerts") };

    let data_dir = android_app
        .internal_data_path()
        .expect("android gives every app an internal data dir");
    slint::android::init(android_app).expect("failed to init slint on android");
    // android keeps everything of an app in its own directory
    let res = run(data_dir.clone(), data_dir);
    // the window is gone (swiped away from the recents) and the app with it, so there is nothing
    // left for the service to keep alive
    notify::set_background(false);
    res.expect("app crashed");
}

# build a debug apk and install it on the connected device, without launching it
installDebug:
    cargo apk build --lib
    adb install -r target/debug/apk/bitcoin-smol.apk

# run the desktop app
desktop:
    cargo run

# serve the web build on http://127.0.0.1:8080, rebuilding on changes
web:
    trunk serve web/index.html

# Contributing to bitcoin-smol

## Code layout

| file              | what                                                                                                                    |
| ---               | ---                                                                                                                     |
| `src/protocol.rs` | event kinds, offer encoding and decoding, the conversation-key DM scheme, pings, reports                                |
| `src/prices.rs`   | hardcoded currencies, price APIs and their fetch functions, premium parsing                                             |
| `src/store.rs`    | key, json state (relays, open offers, ongoing trades, rumor ids, blocklist, labels), redb event database, trust queries |
| `src/form.rs`     | defaults for the new offer form, from the locale and your last offers                                                   |
| `src/deal.rs`     | structured trade messages, and the state of a deal replayed from them                                                   |
| `src/history.rs`  | past trades, in redb                                                                                                    |
| `src/app.rs`      | relay subscriptions, publishing, and the glue to the UI                                                                 |
| `src/app/screenshots.rs` | renders the screenshots in `docs/static/screenshots` offscreen, over made-up data                                |
| `ui/app.slint`    | the interface: market, offer, new offer, trades, trade, profile, people, settings                                       |
| `src/lib.rs`      | Android entry point (`android_main`)                                                                                    |
| `src/main.rs`     | desktop entry point, and the web entry point when built for wasm                                                        |
| `web/index.html`  | the page for the web build, with the canvas the app draws into                                                          |

The protocol (event kinds, tags, the DM scheme, trade reports) is described in `docs/NOSTR.md`.

## Building

On the desktop, for development:

```sh
cargo run                 # data goes in ~/.local/share/bitcoin-smol (%APPDATA% on windows), or set $BITCOIN_SMOL_DATA
cargo test
```

The screenshots in `docs/static/screenshots` come from the real UI, rendered offscreen by Slint's software renderer
over made-up offers, trades and reports, with nothing sent to the network. Run `just screenshots` to update them
after changing the UI.

For Android you need the Android SDK and NDK (`ANDROID_HOME` and `ANDROID_NDK_ROOT` set) and
[cargo-apk](https://crates.io/crates/cargo-apk):

```sh
rustup target add aarch64-linux-android armv7-linux-androideabi
cargo apk run --lib       # builds, installs and launches on a connected device
cargo apk build --lib --release
```

The APK settings (package name, SDK versions and the `INTERNET` permission) are under `[package.metadata.android]` in
`Cargo.toml`.

For the web you need [trunk](https://trunkrs.dev):

```sh
rustup target add wasm32-unknown-unknown
trunk serve web/index.html                                   # on http://127.0.0.1:8080
trunk build web/index.html --release --dist target/web       # a static site, to host anywhere
```

The databases need the origin private file system, which browsers only give to pages served over https or from
localhost. `.github/workflows/pages.yml` publishes the web build at <https://andunieee.github.io/bitcoin-smol/app/>,
next to the user guide.

The user guide lives in `docs/` and is published to <https://andunieee.github.io/bitcoin-smol/> on every push to
`master` (`zola --root docs serve` to preview it).

### Releases

Pushing a `v*` tag runs `.github/workflows/release.yml`. It builds binaries for Linux (x86_64, aarch64), macOS
(aarch64, x86_64) and Windows (x86_64), plus the APK, and attaches them to a GitHub release. The APK is signed with the
keystore in the `ANDROID_KEYSTORE_BASE64` and `ANDROID_KEYSTORE_PASSWORD` repository secrets. Without them it is signed
with a throwaway key, so Android won't install the next release over it.

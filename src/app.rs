//! glue between the ui, the relays and the local store

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use ritualistic::{Event, Filter, ID, Network, PubKey, SecretKey, TagQuery, Timestamp};
use tokio_with_wasm::alias as tokio;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::deal::{self, Action, Deal, OfferTerms, Role, Step};
use crate::form::{self, OfferTemplate};
use crate::history::{OfferMark, Outcome, PastTrade, PublishedReview};
use crate::prices::{self, PriceApi, fiat, thousands};
use crate::protocol::*;
use crate::relays::{MemoryRelay, Occurrence, Relays, Router};
use crate::store::{
    DEFAULT_WRITE_RELAYS, Delivery, ENOUGH_OWN_REVIEWS, Message, MyOffer, Store,
    State, TRUSTED_PEERS_TO_ASK, Trade,
};
use crate::xfk;
use crate::{
    AppWindow, DealRow, DealView, HistoryItem, MsgItem, OfferItem, PastReviewItem, PayItem,
    PersonItem, RelayAnswer, RelayItem, ReviewItem, SentItem, TradeItem, TradeView,
};

const FETCH_TIMEOUT: Duration = Duration::from_secs(12);
const RECONNECT_DELAY: Duration = Duration::from_secs(10);
const PRICE_REFRESH: Duration = Duration::from_secs(60);
/// how far before a trade started its messages are still taken as part of it, for clock skew
const TRADE_START_SLACK: u32 = 5 * 60;
/// how many past trades the trades tab lists
const PAST_TRADES_SHOWN: usize = 30;
/// where we look for the relay lists (nip-65) of the people we trust
const INDEXER_RELAYS: &[&str] = &[
    "wss://purplepag.es",
    "wss://nos.lol",
    "wss://relay.primal.net",
    "wss://indexer.coracle.social",
];

#[cfg(feature = "screenshots")]
pub(crate) mod screenshots;

/// the user guide, built from docs/ by .github/workflows/pages.yml
const DOCS_URL: &str = "https://andunieee.github.io/bitcoin-smol/";

/// a live subscription, one task per relay so each one reconnects on its own. dropping it stops it.
struct LiveSub(Vec<tokio::task::AbortHandle>);

impl Drop for LiveSub {
    fn drop(&mut self) {
        for h in &self.0 {
            h.abort();
        }
    }
}

/// the trade shown in the trade view
#[derive(Debug, Clone, PartialEq, Eq)]
enum TradeRef {
    /// keyed by the pubkey of its conversation
    Ongoing(PubKey),
    /// keyed by its trade id
    Past(String),
}

/// something we published this session, with each relay's answer
#[derive(Clone)]
struct Sent {
    id: u64,
    title: String,
    json: String,
    answers: Vec<Delivery>,
}

/// how many of those are kept
const SENT_KEPT: usize = 200;

/// one entry of the relay report popup
#[derive(Debug, Clone, PartialEq)]
enum Report {
    /// something published this session, by its id
    Sent(u64),
    /// a message of a trade, by its id. ours carry how each relay took it.
    Message(TradeRef, String),
    /// an offer or a review as it came from the relays: what it is, and its json
    Raw(String, String),
}

struct App {
    /// our key and its pubkey. it only changes when an nsec is imported
    identity: RwLock<(SecretKey, PubKey)>,
    /// the real relays, and the one of the XFK practice market
    net: Arc<dyn Relays>,
    xfk_relay: Arc<MemoryRelay>,
    /// the bots of the practice market
    xfk_people: Vec<PubKey>,
    ui: slint::Weak<AppWindow>,
    store: Mutex<Store>,

    offers: Mutex<Vec<Offer>>,
    /// offers of the people we trust, found on their own relays since the last refresh
    trusted_offers: Mutex<Vec<Offer>>,
    /// the search for those, stopped when a new refresh starts one again
    trusted_fetch: Mutex<Option<tokio::task::AbortHandle>>,
    /// (currency, price api) -> btc price
    prices: Mutex<HashMap<(String, String), f64>>,
    /// (currency, price api) -> why the last fetch failed
    price_errors: Mutex<HashMap<(String, String), String>>,
    /// (currency, price api) picked in the new offer form, kept fresh along with the offers' prices
    form_pair: Mutex<(String, String)>,
    locale_currency: String,
    /// (side, currency)
    filters: Mutex<(String, String)>,

    ping_sub: Mutex<Option<LiveSub>>,
    /// whether we had yet to trade for real when the app started: no trade recorded, ongoing or
    /// past, other than in the practice market. then the market and the new offer form start
    /// with XFK.
    practicing: bool,
    /// the practice market is on this device, so its offers show up as they are made
    practice_sub: Mutex<Option<LiveSub>>,
    /// peer hex -> subscription to the conversation pubkey
    dm_subs: Mutex<HashMap<String, LiveSub>>,

    current_trade: Mutex<Option<TradeRef>>,
    current_profile: Mutex<Option<PubKey>>,
    /// held while quoting prices, so a quote isn't sent twice
    quoting: Mutex<()>,
    /// the number of the last offers refresh, so a slow older one doesn't replace a newer one
    refresh_gen: AtomicU64,
    /// what we published this session, and the id for the next one
    sent: Mutex<(Vec<Sent>, u64)>,
    /// what the relay report popup shows, empty while it is closed
    popup: Mutex<Vec<Report>>,
    /// held while looking up or creating our relay list, so we don't publish two
    finding_write_relays: tokio::sync::Mutex<()>,
    /// relays typed into a relay picker or unchecked in one, listed again (unchecked) until the
    /// app closes
    session_relays: Mutex<Vec<String>>,
    /// for the screenshots: nothing is fetched, published or subscribed to
    offline: bool,
}

#[cfg(not(target_arch = "wasm32"))]
pub fn run(
    data_dir: std::path::PathBuf,
    config_dir: std::path::PathBuf,
) -> Result<(), slint::PlatformError> {
    let _ = std::fs::create_dir_all(&data_dir);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start tokio");
    // ritualistic spawns tasks (even on drop), and so do we from the ui callbacks, so the runtime
    // must be reachable from the ui thread
    let _guard = rt.enter();

    let ui = start(Store::open(&data_dir, &config_dir))?;
    ui.run()
}

/// the browser build: everything runs on the page's only thread, and the databases have to be
/// loaded from the origin private file system before anything else
#[cfg(target_arch = "wasm32")]
pub async fn run() -> Result<(), slint::PlatformError> {
    let ui = start(Store::open().await)?;
    // in the browser this returns right away, with the event loop running on its own
    ui.run()?;
    // which would take the window down with it when `ui` is dropped
    std::mem::forget(ui);
    Ok(())
}

fn start(store: Store) -> Result<AppWindow, slint::PlatformError> {
    let (ui, app) = build(store, false)?;

    let files = app.store.lock().unwrap().files.clone();
    xfk::start(app.xfk_relay.clone(), files);
    app.restart_ping_sub();
    app.watch_practice_market();
    let keys: Vec<String> = app
        .store
        .lock()
        .unwrap()
        .state
        .trades
        .keys()
        .cloned()
        .collect();
    for key in keys {
        if let Ok(pk) = key.parse() {
            app.ensure_dm_sub(pk, false);
        }
    }
    app.refresh_offers();

    // offers expire without anything happening, which is when the background service can go
    let a = app.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            a.sync_background();
        }
    });

    let a = app.clone();
    tokio::spawn(async move {
        loop {
            a.fetch_prices().await;
            tokio::time::sleep(PRICE_REFRESH).await;
        }
    });

    Ok(ui)
}

/// the window and the app behind it, before anything goes to the network
fn build(mut store: Store, offline: bool) -> Result<(AppWindow, Arc<App>), slint::PlatformError> {
    let sk = crate::store::load_or_create_key(&*store.config, "key");
    let ui = AppWindow::new()?;
    // the screenshots have nothing to keep
    let xfk_relay = Arc::new(MemoryRelay::new(
        xfk::RELAY,
        (!offline).then(|| store.files.clone()),
    ));
    let xfk_people = xfk::people(&*store.files);
    store.load_trades(&sk, |state, t| trade_relays(state, t, &xfk_people));
    let practicing = !store
        .state
        .trades
        .values()
        .any(|t| real_trade(t.offer.as_ref()))
        && !store.history.any(|t| real_trade(t.offer.as_ref()));
    // the market starts where we last traded, or in the practice market when we never did
    let market_currency = last_trade_currency(&store).unwrap_or_else(|| xfk::XFK.into());

    let app = Arc::new(App {
        identity: RwLock::new((sk.clone(), sk.pubkey())),
        net: Arc::new(Router {
            net: Network::new(),
            memory: xfk_relay.clone(),
        }),
        xfk_relay,
        xfk_people,
        ui: ui.as_weak(),
        store: Mutex::new(store),
        offers: Mutex::new(vec![]),
        trusted_offers: Mutex::new(vec![]),
        trusted_fetch: Mutex::new(None),
        prices: Mutex::new(HashMap::new()),
        price_errors: Mutex::new(HashMap::new()),
        form_pair: Mutex::new(Default::default()),
        locale_currency: form::locale_currency(),
        filters: Mutex::new(("all".into(), market_currency)),
        ping_sub: Mutex::new(None),
        practicing,
        practice_sub: Mutex::new(None),
        dm_subs: Mutex::new(HashMap::new()),
        current_trade: Mutex::new(None),
        current_profile: Mutex::new(None),
        quoting: Mutex::new(()),
        refresh_gen: AtomicU64::new(0),
        sent: Mutex::new((vec![], 0)),
        popup: Mutex::new(vec![]),
        finding_write_relays: tokio::sync::Mutex::new(()),
        session_relays: Mutex::new(vec![]),
        offline,
    });

    setup_ui(&app, &ui);
    Ok((ui, app))
}

fn setup_ui(app: &Arc<App>, ui: &AppWindow) {
    ui.set_npub(app.me().to_npub().into());

    let mut currencies: Vec<SharedString> = vec!["all".into()];
    currencies.extend(prices::currency_codes().into_iter().map(SharedString::from));
    ui.set_create_currencies(ModelRc::new(VecModel::from(currencies[1..].to_vec())));
    ui.set_currencies(ModelRc::new(VecModel::from(currencies)));
    // `currencies` already defaults to ["all"] in the .slint so the filter's initial "all"
    // is in the model from the start (see slint-ui/slint#11970). keep it set here too.
    ui.set_filter_currency(app.filters.lock().unwrap().1.clone().into());

    app.reset_create_form(ui);
    app.render_relays();
    app.render_trades();
    app.render_people();

    let a = app.clone();
    ui.on_refresh_offers(move || a.refresh_offers());

    let a = app.clone();
    ui.on_filters_changed(move || {
        if let Some(ui) = a.ui.upgrade() {
            *a.filters.lock().unwrap() = (
                ui.get_filter_side().to_string(),
                ui.get_filter_currency().to_string(),
            );
        }
        a.render_offers();
    });

    let a = app.clone();
    ui.on_open_offer(move |id| {
        let (item, mine, maker) = {
            let offers = a.offers.lock().unwrap();
            let Some(offer) = offers.iter().find(|o| o.id.to_hex() == id.as_str()) else {
                return;
            };
            (a.offer_item(offer), offer.maker == a.me(), offer.maker)
        };
        // the maker's recent trades, since they back the offer
        let history = if mine { vec![] } else { a.offer_history(maker) };
        if let Some(ui) = a.ui.upgrade() {
            ui.set_current_offer(item);
            ui.set_offer_history(ModelRc::new(VecModel::from(history)));
            ui.set_back_page(0);
            ui.set_page(1);
        }
    });

    let a = app.clone();
    ui.on_contact_maker(move |id| a.contact_maker(&id));

    let a = app.clone();
    ui.on_create_currency_changed(move |cur| {
        if let Some(ui) = a.ui.upgrade() {
            a.create_currency_changed(&ui, &cur);
        }
    });

    let a = app.clone();
    ui.on_create_api_changed(move |api| {
        if let Some(ui) = a.ui.upgrade() {
            a.form_pair_changed(ui.get_create_currency().into(), api.into());
        }
    });

    let a = app.clone();
    ui.on_create_premium_changed(move || a.render_create_preview());

    let weak = ui.as_weak();
    ui.on_toggle_payment(move |name, checked| {
        if let Some(ui) = weak.upgrade() {
            set_checked(&ui.get_payment_options(), &name, checked);
            set_checked(&ui.get_payment_more(), &name, checked);
        }
    });

    let weak = ui.as_weak();
    ui.on_toggle_bitcoin(move |name, checked| {
        if let Some(ui) = weak.upgrade() {
            set_checked(&ui.get_bitcoin_options(), &name, checked);
        }
    });

    ui.on_offer_problem(
        |currency, premium, min, max, other, negotiation, publish, bitcoin, payment| {
            offer_problem(
                &currency,
                &premium,
                &min,
                &max,
                &other,
                &negotiation,
                &publish,
                &bitcoin,
                &payment,
            )
            .unwrap_or_default()
            .into()
        },
    );

    let a = app.clone();
    ui.on_publish_offer(move || a.publish_offer());

    let a = app.clone();
    ui.on_open_trade(move |peer| {
        if let Ok(pk) = peer.parse::<PubKey>() {
            // the trades tab passes the key of a trade, a profile the key of a person
            if a.store
                .lock()
                .unwrap()
                .state
                .trades
                .contains_key(&pk.to_hex())
            {
                a.show_trade(TradeRef::Ongoing(pk));
            } else {
                a.open_trade(pk);
            }
        }
    });

    let a = app.clone();
    ui.on_open_past_trade(move |id| {
        a.show_trade(TradeRef::Past(id.to_string()));
    });

    let a = app.clone();
    ui.on_close_offer(move |id| a.close_offer(&id));

    let a = app.clone();
    ui.on_conclude_trade(move |success, comment| {
        let outcome = if success {
            Outcome::Success
        } else {
            Outcome::Failure
        };
        a.conclude_trade(outcome, comment.trim());
    });

    let a = app.clone();
    ui.on_delete_review(move |id| a.delete_review(&id));

    let a = app.clone();
    ui.on_archive_trade(move || a.archive_trade());

    let a = app.clone();
    ui.on_conclude_now(move || a.end_trade(Outcome::Concluded, ""));

    let a = app.clone();
    ui.on_delete_trade(move || a.delete_trade());

    let a = app.clone();
    ui.on_send_message(move |text| a.send_message(text.trim()));

    let a = app.clone();
    ui.on_trade_pick_amount(move |text| match prices::parse_amount(&text) {
        Some(amount) => a.send_action(Action::Amount(amount)),
        None => a.status("enter an amount, like 50 or 20.5"),
    });

    let a = app.clone();
    ui.on_trade_pick_bitcoin(move |m| a.send_action(Action::BitcoinMethod(m.into())));

    let a = app.clone();
    ui.on_trade_pick_payment(move |m| a.send_action(Action::PaymentMethod(m.into())));

    // whoever gives details gives the ones for what they receive, so there is only one kind each
    let a = app.clone();
    ui.on_trade_give_details(move |details| {
        let details = details.trim().to_string();
        let receives_bitcoin = a
            .current_terms()
            .is_some_and(|t| t.my_role() == t.bitcoin_receiver());
        if !receives_bitcoin {
            a.send_action(Action::PaymentDetails(details));
            return;
        }
        // sent the way it is written down for good, without a uri around it
        match a.check_bitcoin_details(&details) {
            Ok(details) => a.send_action(Action::BitcoinDetails(details)),
            Err(err) => a.status(err),
        }
    });

    let a = app.clone();
    ui.on_trade_check_details(move |details| {
        let receives_bitcoin = a
            .current_terms()
            .is_some_and(|t| t.my_role() == t.bitcoin_receiver());
        if !receives_bitcoin || details.trim().is_empty() {
            return SharedString::new();
        }
        a.check_bitcoin_details(&details)
            .err()
            .unwrap_or_default()
            .into()
    });

    let a = app.clone();
    ui.on_trade_mark_sent(move || a.send_action(Action::Sent));

    let a = app.clone();
    ui.on_trade_quote_price(
        move |text| match prices::parse_amount(&text.replace(',', "")) {
            Some(price) => a.send_action(Action::Price(price)),
            None => a.status("enter the price of one bitcoin, like 61234.5"),
        },
    );

    let a = app.clone();
    ui.on_open_profile(move |who| {
        if let Ok(pk) = who.parse::<PubKey>() {
            a.open_profile(pk);
        }
    });

    let a = app.clone();
    ui.on_review_trade(move |success, comment| a.review_trade(success, comment.trim()));

    let a = app.clone();
    ui.on_set_blocked(move |blocked| {
        let current = *a.current_profile.lock().unwrap();
        if let Some(peer) = current {
            a.set_blocked(peer, blocked);
        }
    });

    let a = app.clone();
    ui.on_set_label(move |label| {
        let current = *a.current_profile.lock().unwrap();
        if let Some(peer) = current {
            a.set_label(peer, label.trim());
        }
    });

    let a = app.clone();
    ui.on_people_filter_changed(move |_| a.render_people());

    let a = app.clone();
    ui.on_relay_toggled(move |which, url, enabled| a.relay_toggled(&which, &url, enabled));

    let a = app.clone();
    ui.on_relay_added(move |which, url| a.relay_added(&which, &url));

    let a = app.clone();
    ui.on_close_sent(move || {
        a.popup.lock().unwrap().clear();
        if let Some(ui) = a.ui.upgrade() {
            ui.set_sent_open(false);
        }
    });

    let a = app.clone();
    ui.on_open_message_report(move |id| {
        let current = a.current_trade.lock().unwrap().clone();
        if let Some(which) = current {
            a.show_report(Report::Message(which, id.into()), true);
        }
    });

    let a = app.clone();
    ui.on_open_raw(move |kind, id| {
        if let Some(report) = a.raw_report(&kind, &id) {
            a.show_report(report, true);
        }
    });

    ui.on_open_relay_site(|host| {
        let url = format!("http://{host}");
        if let Err(err) = webbrowser::open(&url) {
            log::warn!("failed to open {url}: {err}");
        }
    });

    ui.on_open_docs(|| {
        if let Err(err) = webbrowser::open(DOCS_URL) {
            log::warn!("failed to open {DOCS_URL}: {err}");
        }
    });

    let a = app.clone();
    ui.on_import_nsec(move |nsec| a.import_nsec(&nsec));

    let a = app.clone();
    ui.on_reveal_nsec(move || {
        if let Some(ui) = a.ui.upgrade() {
            if ui.get_nsec().is_empty() {
                ui.set_nsec(a.sk().to_nsec().into());
            } else {
                ui.set_nsec("".into());
            }
        }
    });
}

/// how many event ids a subscription remembers
const SEEN_KEPT: usize = 10_000;

/// whether `id` is new. past a limit the memory starts over: what comes again is dropped by the
/// handlers that matter, and a subscription that lives for days shouldn't grow for ever.
fn mark_seen(seen: &mut HashSet<ID>, id: ID) -> bool {
    if seen.len() >= SEEN_KEPT {
        seen.clear();
    }
    seen.insert(id)
}

fn checklist(names: &[&str], checked: &[String]) -> ModelRc<PayItem> {
    ModelRc::new(VecModel::from(
        names
            .iter()
            .map(|name| PayItem {
                name: (*name).into(),
                checked: checked.iter().any(|c| c == name),
            })
            .collect::<Vec<_>>(),
    ))
}

fn set_checked(model: &ModelRc<PayItem>, name: &str, checked: bool) {
    if let Some(idx) = model.iter().position(|p| p.name == name) {
        model.set_row_data(
            idx,
            PayItem {
                name: name.into(),
                checked,
            },
        );
    }
}

/// what is still missing before an offer can be published, for the publish button
#[allow(clippy::too_many_arguments)]
fn offer_problem(
    currency: &str,
    premium: &str,
    min: &str,
    max: &str,
    other_payments: &str,
    relays: &ModelRc<RelayItem>,
    publish_relays: &ModelRc<RelayItem>,
    bitcoin: &ModelRc<PayItem>,
    payment: &ModelRc<PayItem>,
) -> Option<String> {
    if prices::parse_premium(premium).is_none() {
        return Some(
            "the premium is invalid, use a number between -15 and 15, like 1 or -0.5".into(),
        );
    }
    match (prices::parse_amount(min), prices::parse_amount(max)) {
        (Some(min), Some(max)) if min > max => {
            return Some("the minimum amount can't be more than the maximum".into());
        }
        (Some(_), Some(_)) => {}
        _ => return Some(format!("enter the minimum and maximum amount in {currency}")),
    }
    if checked_names(bitcoin).is_empty() {
        return Some("pick at least one way to send bitcoin".into());
    }
    if checked_names(payment).is_empty() && split_list(other_payments).is_empty() {
        return Some("pick at least one payment method".into());
    }
    if !xfk::is_xfk(currency) {
        if selected_relays(relays).is_empty() {
            return Some("pick at least one relay for the negotiation".into());
        }
        if selected_relays(publish_relays).is_empty() {
            return Some("pick at least one relay to publish the offer to".into());
        }
    }
    None
}

/// the relays to pick from, the ones in `selected` checked. whatever is selected but unknown is
/// listed too.
fn relay_items(candidates: &[String], selected: &[String]) -> ModelRc<RelayItem> {
    let mut urls: Vec<&String> = candidates.iter().collect();
    for url in selected {
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    ModelRc::new(VecModel::from(
        urls.into_iter()
            .map(|url| relay_item(url, selected.contains(url)))
            .collect::<Vec<_>>(),
    ))
}

fn relay_item(url: &str, enabled: bool) -> RelayItem {
    RelayItem {
        url: url.into(),
        label: url
            .trim_start_matches("wss://")
            .trim_start_matches("ws://")
            .trim_end_matches('/')
            .into(),
        enabled,
    }
}

/// the relays that are checked
fn selected_relays(model: &ModelRc<RelayItem>) -> Vec<String> {
    model
        .iter()
        .filter(|r| r.enabled && r.url != xfk::RELAY)
        .map(|r| r.url.to_string())
        .collect()
}

fn checked_names(model: &ModelRc<PayItem>) -> Vec<String> {
    model
        .iter()
        .filter(|p| p.checked)
        .map(|p| p.name.to_string())
        .collect()
}

impl App {
    fn sk(&self) -> SecretKey {
        self.identity.read().unwrap().0.clone()
    }

    fn me(&self) -> PubKey {
        self.identity.read().unwrap().1
    }

    /// switch to a key we used before. its reputation comes along, as reviews are public, but
    /// whatever it did on other devices doesn't, so we refuse while this key has trades going on.
    fn import_nsec(self: &Arc<Self>, input: &str) {
        let Ok(sk) = input.trim().parse::<SecretKey>() else {
            self.status("that is not a valid nsec");
            return;
        };
        if sk == self.sk() {
            self.status("that is already your key");
            return;
        }
        let busy = !self.open_offers().is_empty() || {
            let s = &self.store.lock().unwrap().state;
            !s.trades.is_empty()
        };
        if busy {
            self.status(
                "close your open offers and end your ongoing trades before importing a key",
            );
            return;
        }
        {
            let mut store = self.store.lock().unwrap();
            if let Err(err) = store.replace_key(&self.sk(), &sk) {
                self.status(format!("failed to save the key: {err}"));
                return;
            }
            // they belong to the old key, the new one may have its own relay list
            store.state.write_relays = None;
            store.persist();
        }
        *self.identity.write().unwrap() = (sk.clone(), sk.pubkey());

        self.restart_ping_sub();
        self.render_offers();
        self.render_trades();
        let npub = self.me().to_npub();
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_npub(npub.into());
            ui.set_nsec("".into());
        });
        self.status("key imported");
    }

    fn status(&self, msg: impl Into<String>) {
        let msg: String = msg.into();
        log::info!("{msg}");
        let _ = self
            .ui
            .upgrade_in_event_loop(move |ui| ui.set_status(msg.into()));
    }

    /// a system notification about the trade with `peer`, replacing the last one about it.
    /// unless `always`, only while we aren't looking at the app.
    fn notify(&self, peer: &PubKey, title: String, body: String, always: bool) {
        if self.offline {
            return;
        }
        let tag = peer.to_hex();
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            // slint keeps track of it for every backend, but only says so in its private api
            let active =
                slint::private_unstable_api::re_exports::WindowInner::from_pub(ui.window())
                    .active();
            if always || !active {
                crate::notify::show(&title, &body, &tag);
            }
        });
    }

    fn app_relays(&self) -> Vec<String> {
        self.store.lock().unwrap().state.app_relays.clone()
    }

    /// our write relays (nip-65). if we don't know them yet we look for our relay list, and if
    /// there is none and `create`, we publish one with the default relays.
    async fn my_write_relays(self: &Arc<Self>, create: bool) -> Vec<String> {
        if let Some(relays) = &self.store.lock().unwrap().state.write_relays {
            return relays.clone();
        }
        if self.offline {
            return vec![];
        }
        let _finding = self.finding_write_relays.lock().await;
        // found by whoever held the lock before us
        if let Some(relays) = &self.store.lock().unwrap().state.write_relays {
            return relays.clone();
        }

        let me = self.me();
        let indexers: Vec<String> = INDEXER_RELAYS.iter().map(|r| r.to_string()).collect();
        let lists = self
            .fetch(
                indexers.clone(),
                Filter {
                    kinds: Some(vec![KIND_RELAY_LIST]),
                    authors: Some(vec![me]),
                    ..Default::default()
                },
            )
            .await;
        let found = lists
            .iter()
            .filter(|e| e.pubkey == me && e.verify_signature())
            .max_by_key(|e| e.created_at);
        let relays = match found {
            Some(list) => write_relays(list),
            None => {
                // nothing found because nobody answered isn't the same as having no relay list,
                // and we don't want to replace one we couldn't see
                let mut reachable = false;
                for url in &indexers {
                    if self.net.reachable(url).await.unwrap_or(false) {
                        reachable = true;
                        break;
                    }
                }
                if !reachable || !create {
                    return vec![];
                }
                let relays: Vec<String> =
                    DEFAULT_WRITE_RELAYS.iter().map(|r| r.to_string()).collect();
                let evt = relay_list(&self.sk(), &relays);
                let app = self.clone();
                let to = merge_relays(&indexers, &relays);
                // nothing to review about it, it is published along with whatever needed it
                tokio::spawn(async move {
                    let answers = app.deliver(&to, evt, |_| {}).await;
                    log::info!(
                        "relay list published to {}/{} relays",
                        accepted(&answers).len(),
                        answers.len()
                    );
                });
                relays
            }
        };
        if self.me() != me {
            // a key was imported in the meantime
            return vec![];
        }
        let mut store = self.store.lock().unwrap();
        store.state.write_relays = Some(relays.clone());
        store.persist();
        relays
    }

    fn label(&self, pk: &PubKey) -> String {
        let store = self.store.lock().unwrap();
        store
            .state
            .labels
            .get(&pk.to_hex())
            .cloned()
            .unwrap_or_default()
    }

    /// our label for them if we gave them one, otherwise a short npub
    fn name(&self, pk: &PubKey) -> String {
        display_name(&self.store.lock().unwrap().state.labels, pk)
    }

    fn is_blocked(&self, pk: &PubKey) -> bool {
        self.store
            .lock()
            .unwrap()
            .state
            .blocked
            .contains(&pk.to_hex())
    }

    // ------------------------------------------------------------------------------ relays

    /// query relays until all of them send EOSE or a timeout, keeping whatever arrived
    async fn fetch(&self, relays: Vec<String>, filter: Filter) -> Vec<Event> {
        let mut events = vec![];
        if relays.is_empty() || self.offline {
            return events;
        }
        let mut sub = self.net.subscribe(&relays, filter);
        let deadline = tokio::time::sleep(FETCH_TIMEOUT);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                occ = sub.recv() => match occ {
                    Some(Occurrence::Event(evt)) => events.push(evt),
                    _ => break,
                },
                _ = &mut deadline => break,
            }
        }
        events
    }

    /// publish to each relay, calling `on_answer` with all the answers so far as each one arrives.
    /// returns them, none for the relays that never answered.
    async fn deliver(
        &self,
        relays: &[String],
        evt: Event,
        mut on_answer: impl FnMut(&[Delivery]),
    ) -> Vec<Delivery> {
        if self.offline {
            return vec![];
        }
        // invalid urls are skipped without a word, so catch them here, and use the same
        // normalized urls the answers come back with
        let mut urls = vec![];
        let mut answers: Vec<Delivery> = vec![];
        for relay in relays {
            match ritualistic::normalize_url(relay) {
                Ok(url) => {
                    urls.push(url.to_string());
                    answers.push((url.to_string(), None));
                }
                Err(err) => answers.push((relay.clone(), Some(Err(format!("invalid url: {err}"))))),
            }
        }
        on_answer(&answers);

        let mut results = self.net.publish(&urls, evt);
        while let Some((relay_url, answer)) = results.recv().await {
            if let Err(err) = &answer {
                log::warn!("publish to {relay_url} failed: {err}");
            }
            match answers
                .iter_mut()
                .find(|(url, a)| *url == relay_url && a.is_none())
            {
                Some(slot) => slot.1 = Some(answer),
                None => answers.push((relay_url, Some(answer))),
            }
            on_answer(&answers);
        }
        answers
    }

    /// returns the relays that accepted it, and how many we tried. it is kept for the relay
    /// report popup under `title`, which opens on its own when half or more of them failed.
    async fn publish(&self, title: &str, relays: Vec<String>, evt: Event) -> (Vec<String>, usize) {
        let total = relays.len();
        let id = {
            let mut sent = self.sent.lock().unwrap();
            let id = sent.1;
            sent.1 += 1;
            sent.0.push(Sent {
                id,
                title: title.to_string(),
                json: evt.to_string(),
                answers: vec![],
            });
            if sent.0.len() > SENT_KEPT {
                sent.0.remove(0);
            }
            id
        };
        let answers = self
            .deliver(&relays, evt, |answers| {
                if let Some(s) = self.sent.lock().unwrap().0.iter_mut().find(|s| s.id == id) {
                    s.answers = answers.to_vec();
                }
                self.rerender_report(&Report::Sent(id));
            })
            .await;
        if mostly_failed(&answers) {
            self.show_report(Report::Sent(id), false);
        }
        (accepted(&answers), total)
    }

    /// open the relay report popup with `report`, alone or after what it shows already
    fn show_report(&self, report: Report, alone: bool) {
        {
            let mut popup = self.popup.lock().unwrap();
            if alone {
                popup.clear();
            }
            if !popup.contains(&report) {
                popup.push(report);
            }
        }
        self.render_popup();
    }

    /// the raw event of an offer or a review, by its id
    fn raw_report(&self, kind: &str, id: &str) -> Option<Report> {
        let id = ID::from_hex(id).ok()?;
        match kind {
            "offer" => {
                let find = |offers: &Mutex<Vec<Offer>>| {
                    offers.lock().unwrap().iter().find(|o| o.id == id).cloned()
                };
                let o = find(&self.offers).or_else(|| find(&self.trusted_offers))?;
                Some(Report::Raw(
                    format!("offer: {} {}", o.side.as_str(), o.currency),
                    o.raw,
                ))
            }
            "review" => {
                let evt = self
                    .store
                    .lock()
                    .unwrap()
                    .query(Filter {
                        ids: Some(vec![id]),
                        limit: Some(1),
                        ..Default::default()
                    })
                    .into_iter()
                    .next()?;
                let what = if evt.kind == KIND_TRADE_SUCCESS {
                    "successful trade"
                } else {
                    "failed trade"
                };
                Some(Report::Raw(format!("review: {what}"), evt.to_string()))
            }
            _ => None,
        }
    }

    /// refresh the popup if it shows `report`
    fn rerender_report(&self, report: &Report) {
        if self.popup.lock().unwrap().contains(report) {
            self.render_popup();
        }
    }

    /// fill the relay report popup, open while there is anything in it
    fn render_popup(&self) {
        let reviews = self.popup.lock().unwrap().clone();
        let entries: Vec<(String, String, Vec<Delivery>, String)> = reviews
            .iter()
            .filter_map(|r| match r {
                Report::Sent(id) => {
                    let sent = self.sent.lock().unwrap();
                    let s = sent.0.iter().find(|s| s.id == *id)?;
                    Some((
                        s.title.clone(),
                        s.json.clone(),
                        s.answers.clone(),
                        String::new(),
                    ))
                }
                Report::Raw(title, json) => Some((
                    title.clone(),
                    json.clone(),
                    vec![],
                    "the event as it came from the relays.".into(),
                )),
                Report::Message(which, id) => {
                    let m = self.find_message(which.clone(), id)?;
                    let note = if m.mine {
                        if m.deliveries.is_empty() {
                            "no record of how the relays took it."
                        } else {
                            ""
                        }
                    } else {
                        "received from them, so there is nothing of ours to review."
                    };
                    let excerpt: String = m.text.chars().take(80).collect();
                    let title = format!(
                        "{}: {excerpt}{}",
                        if m.mine {
                            "your message"
                        } else {
                            "their message"
                        },
                        if excerpt.len() < m.text.len() {
                            "…"
                        } else {
                            ""
                        }
                    );
                    Some((title, m.event, m.deliveries, note.to_string()))
                }
            })
            .collect();
        let open = !entries.is_empty();
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            let items: Vec<SentItem> = entries
                .into_iter()
                .map(|(title, json, answers, note)| {
                    let answers: Vec<RelayAnswer> = answers
                        .iter()
                        .map(|(url, answer)| RelayAnswer {
                            url: url.into(),
                            host: relay_host(url).into(),
                            state: match answer {
                                None => "sending",
                                Some(Ok(())) => "ok",
                                Some(Err(_)) => "failed",
                            }
                            .into(),
                            error: match answer {
                                Some(Err(err)) => err.into(),
                                _ => "".into(),
                            },
                        })
                        .collect();
                    SentItem {
                        title: title.into(),
                        note: note.into(),
                        json: pretty_json(&json).into(),
                        answers: ModelRc::new(VecModel::from(answers)),
                    }
                })
                .collect();
            ui.set_sent(ModelRc::new(VecModel::from(items)));
            ui.set_sent_open(open);
        });
    }

    /// a message of an ongoing or past trade
    fn find_message(&self, which: TradeRef, id: &str) -> Option<Message> {
        let store = self.store.lock().unwrap();
        match which {
            TradeRef::Ongoing(peer) => store
                .state
                .trades
                .get(&peer.to_hex())?
                .messages
                .iter()
                .find(|m| m.id == id)
                .cloned(),
            TradeRef::Past(row) => store
                .history
                .get(&row)?
                .messages
                .into_iter()
                .find(|m| m.id == id),
        }
    }

    /// keep a filter open on each relay, reconnecting forever, calling `on_event` once per event
    /// with the relay it came from
    fn live(
        self: &Arc<Self>,
        relays: &[String],
        filter: Filter,
        on_event: impl Fn(&Arc<App>, Event, &str) + Send + Sync + 'static,
    ) -> LiveSub {
        if self.offline {
            return LiveSub(vec![]);
        }
        let on_event = Arc::new(on_event);
        let seen = Arc::new(Mutex::new(HashSet::<ID>::new()));
        let handles = relays
            .iter()
            .map(|url| {
                let (app, url, filter) = (self.clone(), url.clone(), filter.clone());
                let (on_event, seen) = (on_event.clone(), seen.clone());
                tokio::spawn(async move {
                    loop {
                        let mut sub = app
                            .net
                            .subscribe(std::slice::from_ref(&url), filter.clone());
                        while let Some(occ) = sub.recv().await {
                            if let Occurrence::Event(evt) = occ
                                && mark_seen(&mut seen.lock().unwrap(), evt.id)
                            {
                                on_event(&app, evt, &url);
                            }
                        }
                        tokio::time::sleep(RECONNECT_DELAY).await;
                    }
                })
                .abort_handle()
            })
            .collect();
        LiveSub(handles)
    }

    fn relays_changed(self: &Arc<Self>) {
        let known = self.known_relays();
        let enabled = self.app_relays();
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            if !xfk::is_xfk(&ui.get_create_currency()) {
                ui.set_publish_relays(relay_items(&known, &enabled));
            }
        });
        self.render_relays();
        self.restart_ping_sub();
        self.refresh_offers();
    }

    /// the relays the pickers list: the defaults, the ones in use and whatever came up since the
    /// app started
    fn known_relays(&self) -> Vec<String> {
        let mut known: Vec<String> = crate::store::DEFAULT_RELAYS.iter().map(|r| r.to_string()).collect();
        let used = self.app_relays();
        let session = self.session_relays.lock().unwrap().clone();
        for url in used.into_iter().chain(session) {
            if !known.contains(&url) {
                known.push(url);
            }
        }
        known
    }

    fn render_relays(&self) {
        let (known, enabled) = (self.known_relays(), self.app_relays());
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_relays(relay_items(&known, &enabled));
        });
    }

    /// fill the relay pickers of the new offer form. the practice market has its own relay.
    fn set_create_relays(&self, ui: &AppWindow, negotiation: &[String], publish: &[String]) {
        let known = self.known_relays();
        ui.set_negotiation_relays(relay_items(&known, negotiation));
        ui.set_publish_relays(relay_items(&known, publish));
    }

    fn remember_session_relays<'a>(&self, urls: impl Iterator<Item = &'a String>) {
        let mut session_relays = self.session_relays.lock().unwrap();
        for url in urls {
            if !session_relays.contains(url) {
                session_relays.push(url.clone());
            }
        }
    }

    fn relay_toggled(self: &Arc<Self>, which: &str, url: &str, enabled: bool) {
        let Some(ui) = self.ui.upgrade() else { return };
        match which {
            "settings" => {
                {
                    let mut store = self.store.lock().unwrap();
                    let relays = &mut store.state.app_relays;
                    relays.retain(|r| r != url);
                    if enabled {
                        relays.push(url.to_string());
                    }
                    store.persist();
                }
                if !enabled {
                    // stays listed, unchecked
                    self.remember_session_relays([&url.to_string()].into_iter());
                }
                self.relays_changed();
            }
            _ => {
                let model = if which == "publish" {
                    ui.get_publish_relays()
                } else {
                    ui.get_negotiation_relays()
                };
                if let Some(idx) = model.iter().position(|r| r.url == url) {
                    model.set_row_data(idx, relay_item(url, enabled));
                }
            }
        }
    }

    fn relay_added(self: &Arc<Self>, which: &str, url: &str) {
        let Some(ui) = self.ui.upgrade() else { return };
        if url.trim().is_empty() {
            return;
        }
        let Ok(url) = ritualistic::normalize_url(url.trim()) else {
            self.status("invalid relay url");
            return;
        };
        let url = url.to_string();
        match which {
            "settings" => {
                {
                    let mut store = self.store.lock().unwrap();
                    if !store.state.app_relays.contains(&url) {
                        store.state.app_relays.push(url);
                    }
                    store.persist();
                }
                self.relays_changed();
            }
            _ => {
                self.remember_session_relays([&url].into_iter());
                let model = if which == "publish" {
                    ui.get_publish_relays()
                } else {
                    ui.get_negotiation_relays()
                };
                match model.iter().position(|r| r.url == url.as_str()) {
                    Some(idx) => model.set_row_data(idx, relay_item(&url, true)),
                    None => {
                        if let Some(vec) = model.as_any().downcast_ref::<VecModel<RelayItem>>() {
                            vec.push(relay_item(&url, true));
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------------------ market

    /// fetch every offer from the app relays, and, at the same time, the offers of the people we
    /// trust from their own relays
    fn refresh_offers(self: &Arc<Self>) {
        self.trusted_offers.lock().unwrap().clear();
        let app = self.clone();
        let handle = tokio::spawn(async move { app.fetch_trusted_offers().await }).abort_handle();
        if let Some(previous) = self.trusted_fetch.lock().unwrap().replace(handle) {
            previous.abort();
        }

        let generation = self.refresh_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let app = self.clone();
        tokio::spawn(async move {
            let _ = app.ui.upgrade_in_event_loop(|ui| ui.set_loading(true));
            let relays = app.app_relays();
            let since = Timestamp(now().0.saturating_sub(OFFER_TTL_SECS));
            let events = app
                .fetch(
                    relays.clone(),
                    Filter {
                        kinds: Some(vec![KIND_OFFER]),
                        since: Some(since),
                        limit: Some(500),
                        ..Default::default()
                    },
                )
                .await;
            if app.refresh_gen.load(Ordering::SeqCst) != generation {
                // a newer refresh is on its way and will say it
                return;
            }

            let mut offers = app.offers_from(&events, since, false);
            // the practice market's are kept up to date as they come, see watch_practice_market
            offers.extend(
                app.offers
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|o| xfk::is_xfk(&o.currency) && o.expiration > now())
                    .cloned(),
            );
            // the trusted ones that arrived first stay
            for o in app.trusted_offers.lock().unwrap().iter() {
                if !offers.iter().any(|x| x.id == o.id) {
                    offers.push(o.clone());
                }
            }
            offers.sort_by_key(|o| std::cmp::Reverse(o.created_at));
            let count = offers.len();
            *app.offers.lock().unwrap() = offers;
            let _ = app.ui.upgrade_in_event_loop(|ui| ui.set_loading(false));
            app.status(format!("{count} offers from {} relays", relays.len()));
            app.render_offers();
            app.fetch_prices().await;
        });
    }

    /// one by one, look up the write relays (nip-65) of each person we trust, and their offers
    /// and trade reviews there. the offers show up in the market as they arrive, the reviews are
    /// kept in the local database.
    async fn fetch_trusted_offers(self: &Arc<Self>) {
        let people = {
            let store = self.store.lock().unwrap();
            store.trusted_people(&self.me(), now().0, &self.xfk_people)
        };
        let indexers: Vec<String> = INDEXER_RELAYS.iter().map(|r| r.to_string()).collect();
        let mut added = false;
        for pk in people {
            let lists = self
                .fetch(
                    indexers.clone(),
                    Filter {
                        kinds: Some(vec![KIND_RELAY_LIST]),
                        authors: Some(vec![pk]),
                        ..Default::default()
                    },
                )
                .await;
            let Some(list) = lists
                .iter()
                .filter(|e| e.pubkey == pk && e.verify_signature())
                .max_by_key(|e| e.created_at)
            else {
                continue;
            };
            let relays = write_relays(list);
            if relays.is_empty() {
                continue;
            }

            let since = Timestamp(now().0.saturating_sub(OFFER_TTL_SECS));
            let (events, reviews) = tokio::join!(
                self.fetch(
                    relays.clone(),
                    Filter {
                        kinds: Some(vec![KIND_OFFER]),
                        authors: Some(vec![pk]),
                        since: Some(since),
                        ..Default::default()
                    },
                ),
                self.fetch(
                    relays,
                    Filter {
                        kinds: Some(vec![KIND_TRADE_FAILURE, KIND_TRADE_SUCCESS]),
                        authors: Some(vec![pk]),
                        limit: Some(500),
                        ..Default::default()
                    },
                ),
            );
            // their reviews, for when we look at the people they traded with
            {
                let store = self.store.lock().unwrap();
                for evt in reviews
                    .iter()
                    .filter(|e| e.pubkey == pk && e.verify_signature())
                {
                    store.save_event(evt);
                }
            }
            let found: Vec<Offer> = self
                .offers_from(&events, since, false)
                .into_iter()
                .filter(|o| o.maker == pk)
                .collect();
            if found.is_empty() {
                continue;
            }
            {
                let mut trusted = self.trusted_offers.lock().unwrap();
                let mut offers = self.offers.lock().unwrap();
                for o in found {
                    if !trusted.iter().any(|x| x.id == o.id) {
                        trusted.push(o.clone());
                    }
                    if !offers.iter().any(|x| x.id == o.id) {
                        offers.push(o);
                        added = true;
                    }
                }
                offers.sort_by_key(|o| std::cmp::Reverse(o.created_at));
            }
            self.render_offers();
        }
        if added {
            self.fetch_prices().await;
        }
    }

    /// follow the offers of the practice market, and their deletions, as they happen: the bots
    /// put theirs up whenever they like, and a refresh is no fun for something this close
    fn watch_practice_market(self: &Arc<Self>) {
        let filter = Filter {
            kinds: Some(vec![KIND_OFFER, KIND_DELETION]),
            since: Some(Timestamp(now().0.saturating_sub(OFFER_TTL_SECS))),
            ..Default::default()
        };
        let sub = self.live(&[xfk::RELAY.into()], filter, |app, evt, _| {
            app.on_practice_event(evt)
        });
        *self.practice_sub.lock().unwrap() = Some(sub);
    }

    fn on_practice_event(self: &Arc<Self>, evt: Event) {
        if evt.kind == KIND_DELETION {
            let ids: Vec<String> = evt.tags.find_all("e").map(|t| t[1].clone()).collect();
            let mut offers = self.offers.lock().unwrap();
            let before = offers.len();
            offers.retain(|o| o.maker != evt.pubkey || !ids.contains(&o.id.to_hex()));
            if offers.len() == before {
                return;
            }
        } else {
            let since = Timestamp(now().0.saturating_sub(OFFER_TTL_SECS));
            let Some(offer) = self.offers_from(&[evt], since, true).pop() else {
                return;
            };
            let pair = (offer.currency.clone(), offer.price_api.clone());
            {
                let mut offers = self.offers.lock().unwrap();
                if offers.iter().any(|o| o.id == offer.id) {
                    return;
                }
                offers.push(offer);
                offers.sort_by_key(|o| std::cmp::Reverse(o.created_at));
            }
            if !self.prices.lock().unwrap().contains_key(&pair) {
                let app = self.clone();
                tokio::spawn(async move { app.fetch_pairs(HashSet::from([pair])).await });
            }
        }
        self.render_offers();
    }

    /// the valid offers among `events`, leaving out blocked makers and our own closed offers.
    /// XFK offers only count from the practice relay (`practice`), and only they count from there.
    fn offers_from(&self, events: &[Event], since: Timestamp, practice: bool) -> Vec<Offer> {
        // our own offers only show while we still have them open, a relay may have
        // ignored the deletion of one we closed
        let mine: HashSet<ID> = self.open_offers().iter().map(|o| o.id).collect();
        events
            .iter()
            .filter(|e| e.created_at >= since && e.verify_signature())
            .filter_map(Offer::from_event)
            .filter(|o| xfk::is_xfk(&o.currency) == practice)
            .filter(|o| !self.is_blocked(&o.maker))
            .filter(|o| o.maker != self.me() || mine.contains(&o.id))
            .collect()
    }

    /// the prices of every offer we show and of what is picked in the new offer form
    async fn fetch_prices(self: &Arc<Self>) {
        let mut pairs: HashSet<(String, String)> = self
            .offers
            .lock()
            .unwrap()
            .iter()
            .map(|o| (o.currency.clone(), o.price_api.clone()))
            .collect();
        pairs.insert(self.form_pair.lock().unwrap().clone());
        pairs.extend(
            self.store
                .lock()
                .unwrap()
                .state
                .trades
                .values()
                .filter_map(|t| t.offer.as_ref())
                .map(|o| (o.currency.clone(), o.price_api.clone())),
        );
        self.fetch_pairs(pairs).await;
    }

    async fn fetch_pairs(self: &Arc<Self>, pairs: HashSet<(String, String)>) {
        if self.offline {
            return;
        }
        let handles: Vec<_> = pairs
            .into_iter()
            .filter_map(|(cur, api_id)| {
                let api = PriceApi::from_id(&api_id)?;
                Some(tokio::spawn(async move {
                    let res = api.fetch(&cur).await;
                    ((cur, api_id), res)
                }))
            })
            .collect();
        for h in handles {
            match h.await {
                Ok((key, Ok(price))) => {
                    self.price_errors.lock().unwrap().remove(&key);
                    self.prices.lock().unwrap().insert(key, price);
                }
                Ok((key, Err(err))) => {
                    log::warn!("price {}/{}: {err}", key.0, key.1);
                    self.price_errors.lock().unwrap().insert(key, err);
                }
                Err(_) => {}
            }
        }
        self.render_offers();
        // also drops our offers that expired since the last time
        self.render_trades();
        self.render_create_preview();
        self.settle_prices();
        self.render_trade();
    }

    fn offer_item(&self, o: &Offer) -> OfferItem {
        let market = self
            .prices
            .lock()
            .unwrap()
            .get(&(o.currency.clone(), o.price_api.clone()))
            .copied();
        let price = match market {
            Some(m) => format!(
                "{} {}/BTC",
                thousands(prices::apply_premium(m, o.premium)),
                o.currency
            ),
            None if PriceApi::from_id(&o.price_api).is_some() => "price loading…".into(),
            None => format!("unknown price api '{}'", o.price_api),
        };
        OfferItem {
            id: o.id.to_hex().into(),
            maker: o.maker.to_npub().into(),
            maker_name: self.name(&o.maker).into(),
            maker_color: key_color(&o.maker),
            mine: o.maker == self.me(),
            side: o.side.as_str().into(),
            currency: o.currency.clone().into(),
            price_api: o.price_api.clone().into(),
            premium: prices::format_premium(o.premium).into(),
            price: price.into(),
            amount: amount_range(o.amount_min, o.amount_max, &o.currency).into(),
            payments: if o.payment_methods.is_empty() {
                "any payment method".to_string()
            } else {
                o.payment_methods.join(" · ")
            }
            .into(),
            bitcoin: if o.bitcoin_methods.is_empty() {
                "not specified".to_string()
            } else {
                o.bitcoin_methods.join(" · ")
            }
            .into(),
            relays: o.relays.join(", ").into(),
            description: o.description.clone().into(),
            age: ago(o.created_at).into(),
        }
    }

    /// the rows shown on the offer page: the last trades with the maker, filled with our most
    /// recent ones with other people when there aren't three of those with the maker.
    fn offer_history(&self, maker: PubKey) -> Vec<HistoryItem> {
        const RECENT: usize = 3;
        let store = self.store.lock().unwrap();
        let labels = &store.state.labels;
        let maker_hex = maker.to_hex();
        // most recently ended first
        let mut rows: Vec<HistoryItem> = vec![];
        for other in [false, true] {
            for t in store.history.list(100) {
                if rows.len() >= RECENT {
                    break;
                }
                // first we take the maker's own trades, then we fill with those of other people
                if (t.peer == maker_hex) != !other {
                    continue;
                }
                let Some(peer) = t.peer.parse::<PubKey>().ok() else {
                    continue;
                };
                // the maker's own rows don't repeat their name, only the filled ones say "with"
                let (name, color) = if t.peer == maker_hex {
                    (SharedString::default(), slint::Color::default())
                } else {
                    (display_name(labels, &peer).into(), key_color(&peer))
                };
                rows.push(HistoryItem {
                    key: t.trade_id.clone().into(),
                    peer_name: name,
                    peer_color: color,
                    outcome: t.outcome.as_str().into(),
                    comment: t.comment.clone().into(),
                    time: ago(Timestamp(t.ended_at)).into(),
                });
            }
        }
        rows
    }

    fn render_offers(&self) {
        let (side, currency) = self.filters.lock().unwrap().clone();
        let items: Vec<OfferItem> = self
            .offers
            .lock()
            .unwrap()
            .iter()
            .filter(|o| match side.as_str() {
                "bitcoin sellers" => o.side == Side::Sell,
                "bitcoin buyers" => o.side == Side::Buy,
                _ => true,
            })
            // the practice market only shows on its own
            .filter(|o| match currency.as_str() {
                "all" | "" => !xfk::is_xfk(&o.currency),
                _ => o.currency == currency,
            })
            .map(|o| self.offer_item(o))
            .collect();
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_offers(ModelRc::new(VecModel::from(items)));
        });
    }

    // ------------------------------------------------------------------------------ new offer

    /// fill the form with whatever our last offers had in common, or the locale's currency and
    /// some free relays when we never made one
    fn reset_create_form(self: &Arc<Self>, ui: &AppWindow) {
        let recent = self.store.lock().unwrap().state.recent_offers.clone();
        // newcomers make their first offers in the practice market
        let fallback = if self.practicing {
            xfk::XFK
        } else {
            &self.locale_currency
        };
        let d = form::defaults(&recent, fallback);
        self.set_create_relays(ui, &d.relays, &self.app_relays());
        ui.set_create_side(d.side.into());
        ui.set_create_currency(d.currency.clone().into());
        ui.set_create_premium(
            prices::format_premium(d.premium)
                .trim_end_matches('%')
                .into(),
        );
        ui.set_bitcoin_options(checklist(BITCOIN_METHODS, &d.bitcoin_methods));
        ui.set_create_description("".into());
        self.create_currency_changed(ui, &d.currency);
    }

    /// price apis and payment methods depend on the currency
    fn create_currency_changed(self: &Arc<Self>, ui: &AppWindow, currency: &str) {
        let recent = self.store.lock().unwrap().state.recent_offers.clone();

        let api = form::default_api(&recent, currency);
        let apis: Vec<SharedString> = prices::apis_for(currency)
            .iter()
            .map(|a| a.id().into())
            .collect();
        ui.set_create_apis(ModelRc::new(VecModel::from(apis)));
        ui.set_create_api(api.into());

        let checked = form::default_payment_methods(&recent, currency);
        let (mut common, mut more) = payment_methods_for(currency);
        // something we picked before for this currency is common enough for us
        let (picked, rest): (Vec<_>, Vec<_>) = more
            .into_iter()
            .partition(|m| checked.iter().any(|c| c == m));
        common.extend(picked);
        more = rest;
        let others: Vec<&str> = checked
            .iter()
            .map(|c| c.as_str())
            .filter(|c| !PAYMENT_METHODS.iter().any(|(m, _)| m == c))
            .collect();
        ui.set_payment_options(checklist(&common, &checked));
        ui.set_payment_more(checklist(&more, &checked));
        ui.set_create_other_payments(others.join(", ").into());

        // the practice market is on a relay of its own, the fields go back to normal after it
        if xfk::is_xfk(currency) {
            let practice = || ModelRc::new(VecModel::from(vec![relay_item(xfk::RELAY, true)]));
            ui.set_negotiation_relays(practice());
            ui.set_publish_relays(practice());
        } else if ui.get_negotiation_relays().iter().any(|r| r.url == xfk::RELAY) {
            let d = form::defaults(&recent, &self.locale_currency);
            self.set_create_relays(ui, &d.relays, &self.app_relays());
        }

        let (min, max) = match form::default_amount(&recent, currency) {
            Some((min, max)) => (prices::format_amount(min), prices::format_amount(max)),
            None => Default::default(),
        };
        ui.set_create_amount_linked(min == max);
        ui.set_create_amount_min(min.into());
        ui.set_create_amount_max(max.into());

        self.form_pair_changed(currency.into(), api.into());
    }

    fn form_pair_changed(self: &Arc<Self>, currency: String, api: String) {
        let pair = (currency, api);
        *self.form_pair.lock().unwrap() = pair.clone();
        if !self.prices.lock().unwrap().contains_key(&pair) {
            let app = self.clone();
            tokio::spawn(async move { app.fetch_pairs(HashSet::from([pair])).await });
        }
        self.render_create_preview();
    }

    fn render_create_preview(self: &Arc<Self>) {
        let app = self.clone();
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            let cur = ui.get_create_currency().to_string();
            let api = ui.get_create_api().to_string();
            let key = (cur.clone(), api.clone());
            let text = match prices::parse_premium(&ui.get_create_premium()) {
                None => {
                    "invalid premium, use a number between -15 and 15, like 1 or -0.5".to_string()
                }
                Some(premium) => match app.prices.lock().unwrap().get(&key) {
                    Some(m) => format!(
                        "market {} {cur}/BTC ({api}) → your price {} {cur}/BTC",
                        thousands(*m),
                        thousands(prices::apply_premium(*m, premium))
                    ),
                    None => match app.price_errors.lock().unwrap().get(&key) {
                        Some(err) => format!("failed to fetch the {api} price: {err}"),
                        None => "fetching the market price…".into(),
                    },
                },
            };
            ui.set_create_preview(text.into());
        });
    }

    fn publish_offer(self: &Arc<Self>) {
        let Some(ui) = self.ui.upgrade() else { return };

        let Some(premium) = prices::parse_premium(&ui.get_create_premium()) else {
            self.status("invalid premium, use a number between -15 and 15, like 1 or -0.5");
            return;
        };
        let currency = ui.get_create_currency().to_string();
        let (Some(amount_min), Some(amount_max)) = (
            prices::parse_amount(&ui.get_create_amount_min()),
            prices::parse_amount(&ui.get_create_amount_max()),
        ) else {
            self.status(format!(
                "enter the minimum and maximum amount in {currency}"
            ));
            return;
        };
        if amount_min > amount_max {
            self.status("the minimum amount can't be more than the maximum");
            return;
        }
        let bitcoin_methods = checked_names(&ui.get_bitcoin_options());
        if bitcoin_methods.is_empty() {
            self.status("pick at least one way to send bitcoin");
            return;
        }
        let mut payment_methods = checked_names(&ui.get_payment_options());
        payment_methods.extend(split_list(&ui.get_create_other_payments()));
        if payment_methods.is_empty() {
            self.status("pick at least one payment method");
            return;
        }
        let practice = xfk::is_xfk(&currency);
        let relays: Vec<String> = if practice {
            vec![xfk::RELAY.into()]
        } else {
            selected_relays(&ui.get_negotiation_relays())
        };
        if relays.is_empty() {
            self.status("pick at least one relay for the negotiation");
            return;
        }
        let publish_to: Vec<String> = if practice {
            vec![xfk::RELAY.into()]
        } else {
            selected_relays(&ui.get_publish_relays())
        };
        if publish_to.is_empty() {
            self.status("pick at least one relay to publish the offer to");
            return;
        }
        if !practice {
            self.remember_session_relays(relays.iter().chain(&publish_to));
        }

        let template = OfferTemplate {
            side: ui.get_create_side().into(),
            currency: currency.clone(),
            price_api: ui.get_create_api().into(),
            premium,
            amount: Some((amount_min, amount_max)),
            payment_methods: payment_methods.clone(),
            bitcoin_methods: bitcoin_methods.clone(),
            relays: relays.clone(),
        };
        let evt = NewOffer {
            side: if ui.get_create_side() == "buy" {
                Side::Buy
            } else {
                Side::Sell
            },
            currency,
            price_api: ui.get_create_api().to_string(),
            premium,
            amount_min,
            amount_max,
            payment_methods,
            bitcoin_methods,
            relays: relays.clone(),
            description: ui.get_create_description().trim().to_string(),
        }
        .to_event(&self.sk());
        if let Some(offer) = Offer::from_event(&evt) {
            self.offers.lock().unwrap().insert(0, offer);
        }
        self.render_offers();

        // start listening for pings on the negotiation relays right away
        {
            let mut store = self.store.lock().unwrap();
            store.state.my_offers.push(MyOffer {
                event: evt.clone(),
                relays: vec![],
            });
            // practice offers say nothing about how we trade for real
            if !practice {
                let recent = &mut store.state.recent_offers;
                recent.insert(0, template);
                recent.truncate(form::RECENT_OFFERS);
            }
            let ping_relays = &mut store.state.ping_relays;
            for r in relays.into_iter().rev() {
                ping_relays.retain(|x| x != &r);
                ping_relays.insert(0, r);
            }
            ping_relays.truncate(12);
            store.persist();
        }
        self.restart_ping_sub();

        self.reset_create_form(&ui);
        self.render_trades();
        ui.set_page(0);
        // to hear about takers
        crate::notify::request_permission();
        self.status("publishing offer…");

        let app = self.clone();
        tokio::spawn(async move {
            let id = evt.id;
            // a practice offer never makes it to our relay list, or anywhere real
            let relays = if practice {
                publish_to
            } else {
                merge_relays(&publish_to, &app.my_write_relays(true).await)
            };
            let (ok, total) = app.publish("offer", relays, evt).await;
            app.status(format!("offer published to {}/{total} relays", ok.len()));
            let mut store = app.store.lock().unwrap();
            if let Some(o) = store.state.my_offers.iter_mut().find(|o| o.event.id == id) {
                o.relays = ok;
                store.persist();
            }
        });
    }

    // ------------------------------------------------------------------------------ trades

    /// our offers that haven't expired yet. expired ones are dropped from the state for good.
    fn open_offers(&self) -> Vec<Offer> {
        let mut store = self.store.lock().unwrap();
        let before = store.state.my_offers.len();
        store
            .state
            .my_offers
            .retain(|o| Offer::from_event(&o.event).is_some());
        if store.state.my_offers.len() != before {
            store.persist();
        }
        store
            .state
            .my_offers
            .iter()
            .filter_map(|o| Offer::from_event(&o.event))
            .collect()
    }

    /// nip-09 deletion, sent to the relays that took the offer
    fn close_offer(self: &Arc<Self>, offer_id: &str) {
        let Some(mine) = ({
            let mut store = self.store.lock().unwrap();
            let pos = store
                .state
                .my_offers
                .iter()
                .position(|o| o.event.id.to_hex() == offer_id);
            let mine = pos.map(|p| store.state.my_offers.remove(p));
            store.persist();
            mine
        }) else {
            return;
        };
        self.offers
            .lock()
            .unwrap()
            .retain(|o| o.id.to_hex() != offer_id);
        self.render_offers();
        self.render_trades();

        let practice = mine
            .event
            .tags
            .find("currency")
            .is_some_and(|t| xfk::is_xfk(&t[1]));
        let relays = if practice {
            vec![xfk::RELAY.into()]
        } else if mine.relays.is_empty() {
            self.app_relays()
        } else {
            mine.relays
        };
        let evt = deletion(&self.sk(), &mine.event.id, KIND_OFFER);
        let app = self.clone();
        tokio::spawn(async move {
            let (ok, total) = app.publish("offer deletion", relays, evt).await;
            app.status(format!("offer deleted from {}/{total} relays", ok.len()));
        });
    }

    /// pings are signed by the sender's real key and p-tag us. we listen for them on our app relays
    /// and on the negotiation relays of the offers we made.
    fn restart_ping_sub(self: &Arc<Self>) {
        let relays = {
            let s = &self.store.lock().unwrap().state;
            let mut relays = s.app_relays.clone();
            for r in &s.ping_relays {
                if !relays.contains(r) {
                    relays.push(r.clone());
                }
            }
            relays
        };
        let filter = Filter {
            kinds: Some(vec![KIND_PING]),
            tags: Some(vec![TagQuery("p".into(), vec![self.me().to_hex()])]),
            since: Some(now()),
            ..Default::default()
        };
        let sub = self.live(&relays, filter, |app, evt, url| app.on_ping(evt, url));
        *self.ping_sub.lock().unwrap() = Some(sub);
    }

    /// someone wants to trade with us: that starts a trade, unless there is one going on already.
    /// `url` is the relay it came through.
    fn on_ping(self: &Arc<Self>, evt: Event, url: &str) {
        let peer = evt.pubkey;
        if peer == self.me() || !evt.verify_signature() || self.is_blocked(&peer) {
            return;
        }
        let trade_id = ping_trade_id(&evt);
        let key = Conversation::new(&self.sk(), peer, &trade_id).pubkey();
        let is_new = {
            let mut store = self.store.lock().unwrap();
            let is_new = !store.state.trades.contains_key(&key.to_hex());
            if is_new {
                // the practice market and the real one never mix
                let relays = if url == xfk::RELAY {
                    vec![xfk::RELAY.into()]
                } else {
                    let ping_relays: Vec<String> = store
                        .state
                        .ping_relays
                        .iter()
                        .filter(|r| *r != xfk::RELAY)
                        .cloned()
                        .collect();
                    if ping_relays.is_empty() {
                        store.state.app_relays.clone()
                    } else {
                        ping_relays
                    }
                };
                store.state.trades.insert(
                    key.to_hex(),
                    Trade {
                        peer: peer.to_hex(),
                        trade_id,
                        relays,
                        offer_summary: "pinged you about your offer".into(),
                        started_at: evt.created_at.0,
                        started_by_me: false,
                        ping_saved: true,
                        ..Default::default()
                    },
                );
                store.save_event(&evt);
            }
            is_new
        };
        self.ensure_dm_sub(key, false);
        if is_new {
            let name = self.name(&peer);
            self.status(format!("new trade with {name}"));
            self.notify(
                &key,
                format!("{name} wants to trade"),
                "they contacted you, likely about one of your offers".into(),
                true,
            );
            self.render_trades();
        }
    }

    /// subscribe to `authors:[conversation pubkey]`, which is all it takes to hear from a peer.
    /// blocking someone is just never doing this for them. only ongoing trades are listened to.
    /// `key` is the conversation pubkey of the trade.
    fn ensure_dm_sub(self: &Arc<Self>, key: PubKey, restart: bool) {
        let hex = key.to_hex();
        let mut subs = self.dm_subs.lock().unwrap();
        if subs.contains_key(&hex) && !restart {
            return;
        }
        let Some((peer, trade_id, relays, started_at, slack)) = self
            .store
            .lock()
            .unwrap()
            .state
            .trades
            .get(&hex)
            .and_then(|t| {
                Some((
                    t.peer.parse::<PubKey>().ok()?,
                    t.trade_id.clone(),
                    t.relays.clone(),
                    t.started_at,
                    start_slack(t),
                ))
            })
        else {
            return;
        };
        if self.is_blocked(&peer) {
            return;
        }
        let conv = Conversation::new(&self.sk(), peer, &trade_id);
        let filter = Filter {
            kinds: Some(vec![KIND_DM]),
            authors: Some(vec![conv.pubkey()]),
            // dms stay on relays for a while, older ones belong to an earlier trade
            since: (started_at > 0).then(|| Timestamp(started_at.saturating_sub(slack))),
            ..Default::default()
        };
        let sub = self.live(&relays, filter, move |app, evt, _| app.on_dm(&conv, evt));
        subs.insert(hex, sub);
    }

    fn on_dm(self: &Arc<Self>, conv: &Conversation, evt: Event) {
        let Some(rumor) = conv.unwrap(&evt) else {
            return;
        };
        let mut new_pair = None;
        let mut offer_mark = None;
        let shown = {
            let mut store = self.store.lock().unwrap();
            // the taker's messages point at the offer they took, which is how we, the maker,
            // learn which of our offers this trade is about
            let offer = rumor
                .tags
                .iter()
                .find(|t| t.len() >= 2 && t[0] == "e")
                .and_then(|t| {
                    store
                        .state
                        .my_offers
                        .iter()
                        .find(|o| o.event.id.to_hex() == t[1])
                })
                .and_then(|o| Offer::from_event(&o.event));
            let Some(t) = store.state.trades.get_mut(&conv.pubkey().to_hex()) else {
                return;
            };
            // whoever signed this could have been either of us, the rumor can't say. our own
            // messages are in the trade already, which is how we know them when the relays send
            // them back.
            let theirs = conv.local_event(&rumor, &conv.peer);
            let ours = conv.local_event(&rumor, &self.me());
            if t.messages
                .iter()
                .any(|m| m.id == ours.id.to_hex() || m.id == theirs.id.to_hex())
                || rumor.created_at < t.started_at.saturating_sub(start_slack(t))
            {
                return;
            }
            if t.offer.is_none()
                && let Some(offer) = offer
            {
                t.offer_id = Some(offer.id.to_hex());
                t.offer_summary = offer_summary(&offer);
                let terms = OfferTerms::from_offer(&offer, true);
                offer_mark = Some(offer_mark_event(&self.me(), conv, &offer, &terms, t));
                t.offer = Some(terms);
                new_pair = Some((offer.currency, offer.price_api));
            }
            let action = rumor
                .tags
                .iter()
                .find(|t| t.first().is_some_and(|n| n == deal::TAG))
                .cloned();
            t.messages.push(Message {
                id: theirs.id.to_hex(),
                created_at: rumor.created_at,
                mine: false,
                text: rumor.content,
                action,
                event: theirs.to_string(),
                deliveries: vec![],
            });
            t.messages.sort_by_key(|m| m.created_at);
            let shown = t
                .messages
                .iter()
                .zip(shown_messages(t.offer.as_ref(), &t.messages))
                .find(|(m, _)| m.id == theirs.id.to_hex())
                .map(|(_, shown)| shown.text);
            let summary = t.offer_summary.clone();
            store.save_event(&theirs);
            if let Some(mark) = &offer_mark {
                store.save_event(mark);
            }
            (shown, summary)
        };
        let name = self.name(&conv.peer);
        if new_pair.is_some() {
            self.notify(
                &conv.pubkey(),
                format!("{name} took your offer"),
                shown.1,
                true,
            );
        } else if let Some(text) = shown.0 {
            self.notify(&conv.pubkey(), name, text, false);
        }
        // we need the price of an offer to quote it to its taker
        if let Some(pair) = new_pair.filter(|p| !self.prices.lock().unwrap().contains_key(p)) {
            let app = self.clone();
            tokio::spawn(async move { app.fetch_pairs(HashSet::from([pair])).await });
        }
        self.settle_prices();
        self.render_trades();
        if *self.current_trade.lock().unwrap() == Some(TradeRef::Ongoing(conv.pubkey())) {
            self.render_trade();
        } else {
            self.status(format!("new message from {}", self.name(&conv.peer)));
        }
    }

    fn contact_maker(self: &Arc<Self>, offer_id: &str) {
        let Some(offer) = self
            .offers
            .lock()
            .unwrap()
            .iter()
            .find(|o| o.id.to_hex() == offer_id)
            .cloned()
        else {
            return;
        };
        if offer.maker == self.me() {
            return;
        }
        if self.is_blocked(&offer.maker) {
            self.status("you blocked this maker");
            return;
        }
        let key = {
            let mut store = self.store.lock().unwrap();
            // taking the same offer again while that trade goes on is the same trade
            let existing = store
                .state
                .trades
                .iter()
                .find(|(_, t)| {
                    t.started_by_me
                        && t.peer == offer.maker.to_hex()
                        && t.offer_id.as_deref() == Some(offer.id.to_hex().as_str())
                })
                .and_then(|(k, _)| k.parse::<PubKey>().ok());
            let key = existing.unwrap_or_else(|| {
                let trade_id = new_trade_id();
                let key = Conversation::new(&self.sk(), offer.maker, &trade_id).pubkey();
                store.state.trades.insert(
                    key.to_hex(),
                    Trade {
                        peer: offer.maker.to_hex(),
                        trade_id,
                        started_at: now().0,
                        started_by_me: true,
                        ..Default::default()
                    },
                );
                key
            });
            let t = store
                .state
                .trades
                .get_mut(&key.to_hex())
                .expect("just inserted or found");
            t.relays = if xfk::is_xfk(&offer.currency) {
                vec![xfk::RELAY.into()]
            } else {
                offer.relays.clone()
            };
            t.offer_id = Some(offer.id.to_hex());
            t.offer_summary = offer_summary(&offer);
            let terms = OfferTerms::from_offer(&offer, false);
            let mark = offer_mark_event(
                &self.me(),
                &Conversation::new(&self.sk(), offer.maker, &t.trade_id),
                &offer,
                &terms,
                t,
            );
            t.offer = Some(terms);
            store.save_event(&mark);
            key
        };
        self.ensure_dm_sub(key, true);
        self.send_ping(key);
        // to hear back from them
        crate::notify::request_permission();
        // there is nothing to pick about a fixed amount, but the maker answers it with the price
        let terms = OfferTerms::from_offer(&offer, false);
        if terms.fixed_amount() {
            self.send_action_to(key, Action::Amount(offer.amount_min), true);
        }
        self.render_trades();
        self.show_trade(TradeRef::Ongoing(key));
    }

    /// ping the other side of the trade `key`, which tells them how to find its conversation
    fn send_ping(self: &Arc<Self>, key: PubKey) {
        let (peer, trade_id, relays, evt) = {
            let mut store = self.store.lock().unwrap();
            let Some(t) = store.state.trades.get_mut(&key.to_hex()) else {
                return;
            };
            let Ok(peer) = t.peer.parse::<PubKey>() else {
                return;
            };
            let evt = ping(&self.sk(), &peer, &t.trade_id);
            let (trade_id, relays, saved) = (t.trade_id.clone(), t.relays.clone(), t.ping_saved);
            t.ping_saved = true;
            // the first ping is what brings the trade back after a restart
            if !saved {
                store.save_event(&evt);
            }
            (peer, trade_id, relays, evt)
        };
        let _ = (peer, trade_id);
        let app = self.clone();
        tokio::spawn(async move {
            app.publish("ping", relays, evt).await;
        });
    }

    /// the conversation pubkey of the most recent ongoing trade with `peer`
    fn latest_trade_with(&self, peer: &PubKey) -> Option<PubKey> {
        let store = self.store.lock().unwrap();
        let hex = peer.to_hex();
        store
            .state
            .trades
            .iter()
            .filter(|(_, t)| t.peer == hex)
            .max_by_key(|(_, t)| t.messages.last().map_or(t.started_at, |m| m.created_at))
            .and_then(|(k, _)| k.parse().ok())
    }

    /// the conversation pubkeys of all the ongoing trades with `peer`
    fn trades_with(&self, peer: &PubKey) -> Vec<PubKey> {
        let hex = peer.to_hex();
        self.store
            .lock()
            .unwrap()
            .state
            .trades
            .iter()
            .filter(|(_, t)| t.peer == hex)
            .filter_map(|(k, _)| k.parse().ok())
            .collect()
    }

    /// open the ongoing trade with someone, starting a direct chat if there is none
    fn open_trade(self: &Arc<Self>, peer: PubKey) {
        if peer == self.me() {
            return;
        }
        let (key, created) = match self.latest_trade_with(&peer) {
            Some(key) => (key, false),
            None => {
                let mut store = self.store.lock().unwrap();
                // talking to someone outside of an offer, use our own relays. the bots are only
                // ever in the practice market.
                let relays = if self.xfk_people.contains(&peer) {
                    vec![xfk::RELAY.into()]
                } else {
                    store.state.app_relays.clone()
                };
                let trade_id = new_trade_id();
                let key = Conversation::new(&self.sk(), peer, &trade_id).pubkey();
                store.state.trades.insert(
                    key.to_hex(),
                    Trade {
                        peer: peer.to_hex(),
                        trade_id,
                        relays,
                        // the peer's name stands within the header line, so say it as a detail
                        offer_summary: "· direct chat".into(),
                        started_at: now().0,
                        started_by_me: true,
                        ..Default::default()
                    },
                );
                store.persist();
                (key, true)
            }
        };
        if created {
            self.ensure_dm_sub(key, false);
            self.send_ping(key);
            self.render_trades();
        }
        self.show_trade(TradeRef::Ongoing(key));
    }

    fn show_trade(self: &Arc<Self>, which: TradeRef) {
        *self.current_trade.lock().unwrap() = Some(which);
        self.render_trade();
        // the price is usually there already, from the offer in the market
        if let Some(t) = self.current_terms() {
            let pair = (t.currency, t.price_api);
            if !self.prices.lock().unwrap().contains_key(&pair) {
                let app = self.clone();
                tokio::spawn(async move { app.fetch_pairs(HashSet::from([pair])).await });
            }
        }
        let _ = self.ui.upgrade_in_event_loop(|ui| {
            ui.set_trade_confirm_delete(false);
            ui.set_trade_concluding(false);
            ui.set_trade_confirming("".into());
            ui.set_trade_details_draft("".into());
            ui.set_back_page(3);
            ui.set_page(4);
        });
    }

    fn render_trade(&self) {
        let Some(which) = self.current_trade.lock().unwrap().clone() else {
            return;
        };
        let store = self.store.lock().unwrap();
        let (peer, view, reviews) = match which {
            TradeRef::Ongoing(peer) => {
                let Some(t) = store.state.trades.get(&peer.to_hex()) else {
                    return;
                };
                let peer = t.peer.parse().unwrap_or(peer);
                let view = TradeView {
                    ongoing: true,
                    // stored summaries may pre-date the "maker " word being shown inline, drop it
                    summary: t.offer_summary.trim_start_matches("maker ").into(),
                    ..Default::default()
                };
                (
                    peer,
                    (view, t.offer.clone(), t.messages.clone(), t.price_check),
                    vec![],
                )
            }
            TradeRef::Past(id) => {
                let Some(t) = store.history.get(&id) else {
                    return;
                };
                let Ok(peer) = t.peer.parse::<PubKey>() else {
                    return;
                };
                let reviews: Vec<PastReviewItem> = t
                    .reviews
                    .iter()
                    .map(|r| PastReviewItem {
                        id: r.id.clone().into(),
                        success: r.success,
                        comment: r.comment.clone().into(),
                        time: ago(Timestamp(r.created_at)).into(),
                    })
                    .collect();
                let view = TradeView {
                    ongoing: false,
                    pending: t.outcome == Outcome::Concluded,
                    summary: t.offer_summary.trim_start_matches("maker ").into(),
                    ..Default::default()
                };
                (peer, (view, t.offer, t.messages, None), reviews)
            }
        };
        let (mut view, terms, messages, check) = view;
        let shown = shown_messages(terms.as_ref(), &messages);
        let deal = terms.map(|terms| {
            let deal = Deal::replay(&terms, &messages);
            // only for ongoing trades, today's price says nothing about a past one
            let live = view.ongoing.then(|| self.trade_price(&terms));
            (terms, deal, live)
        });
        let s = &store.state;
        view.peer = peer.to_npub().into();
        view.peer_name = display_name(&s.labels, &peer).into();
        view.peer_color = key_color(&peer);
        view.blocked = s.blocked.contains(&peer.to_hex());
        drop(store);
        let msgs: Vec<MsgItem> = messages
            .iter()
            .zip(shown)
            .map(|(m, shown)| MsgItem {
                id: m.id.clone().into(),
                mine: m.mine,
                text: shown.text.into(),
                meta: shown.meta,
                ignored: shown.ignored,
                time: match delivery_summary(&m.deliveries) {
                    Some(d) => format!("{} · {d}", ago(Timestamp(m.created_at))),
                    None => ago(Timestamp(m.created_at)),
                }
                .into(),
            })
            .collect();
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_trade(view);
            ui.set_deal(match deal {
                Some((terms, deal, live)) => deal_view(&terms, &deal, live, check),
                None => DealView::default(),
            });
            ui.set_messages(ModelRc::new(VecModel::from(msgs)));
            ui.set_trade_reviews(ModelRc::new(VecModel::from(reviews)));
        });
    }

    fn send_message(self: &Arc<Self>, text: &str) {
        if text.is_empty() || text.len() > 8000 {
            return;
        }
        let Some(TradeRef::Ongoing(peer)) = self.current_trade.lock().unwrap().clone() else {
            return;
        };
        self.send(peer, text, None);
    }

    /// what a bitcoin costs in a trade: the market price from the offer's source plus its premium.
    /// or why we don't know.
    fn trade_price(&self, terms: &OfferTerms) -> Result<f64, String> {
        let key = (terms.currency.clone(), terms.price_api.clone());
        if let Some(market) = self.prices.lock().unwrap().get(&key) {
            return Ok(prices::apply_premium(*market, terms.premium));
        }
        if PriceApi::from_id(&terms.price_api).is_none() {
            return Err(format!("unknown price source '{}'", terms.price_api));
        }
        match self.price_errors.lock().unwrap().get(&key) {
            Some(err) => Err(format!(
                "failed to fetch the {} price: {err}",
                terms.price_api
            )),
            None => Err("fetching the market price…".into()),
        }
    }

    /// the terms of the offer behind the ongoing trade on screen, if it came from one
    fn current_terms(&self) -> Option<OfferTerms> {
        let Some(TradeRef::Ongoing(peer)) = self.current_trade.lock().unwrap().clone() else {
            return None;
        };
        let store = self.store.lock().unwrap();
        store.state.trades.get(&peer.to_hex())?.offer.clone()
    }

    /// where the bitcoin of the trade on screen should go, checked for its method, or why it
    /// can't go there
    fn check_bitcoin_details(&self, details: &str) -> Result<String, String> {
        let Some(TradeRef::Ongoing(peer)) = self.current_trade.lock().unwrap().clone() else {
            return Err("no ongoing trade".into());
        };
        let store = self.store.lock().unwrap();
        let t = store
            .state
            .trades
            .get(&peer.to_hex())
            .ok_or("no ongoing trade")?;
        let terms = t.offer.as_ref().ok_or("this chat isn't about an offer")?;
        let deal = Deal::replay(terms, &t.messages);
        let method = deal.bitcoin_method.as_deref().unwrap_or("");
        crate::address::parse(method, details, deal.sats())
    }

    /// a structured message in the trade on screen, from a button
    fn send_action(self: &Arc<Self>, action: Action) {
        let Some(TradeRef::Ongoing(peer)) = self.current_trade.lock().unwrap().clone() else {
            return;
        };
        if self.send_action_to(peer, action, false) {
            let _ = self.ui.upgrade_in_event_loop(|ui| {
                ui.set_trade_confirming("".into());
                ui.set_trade_details_draft("".into());
            });
        }
    }

    /// a structured message: its text is made up from the action, which goes in a tag.
    /// `quiet` is for the ones the app sends on its own, where a refusal is no news.
    /// returns whether it was sent.
    fn send_action_to(self: &Arc<Self>, peer: PubKey, action: Action, quiet: bool) -> bool {
        let text = {
            let store = self.store.lock().unwrap();
            let Some(terms) = store
                .state
                .trades
                .get(&peer.to_hex())
                .and_then(|t| Some((t.offer.as_ref()?, &t.messages)))
                .map(|(terms, messages)| (terms.clone(), Deal::replay(terms, messages)))
            else {
                return false;
            };
            let (terms, deal) = terms;
            let me = terms.my_role();
            if let Err(err) = deal.check(&terms, me, &action) {
                drop(store);
                if !quiet {
                    self.status(err);
                }
                return false;
            }
            action.text(&terms, me, &deal)
        };
        if text.len() > 8000 {
            self.status("that is too long");
            return false;
        }
        self.send(peer, &text, Some(action));
        true
    }

    /// the maker's price is the one that counts, so as the maker we quote it as soon as we know
    /// the amount and the market price. as the taker we check it against the market price from
    /// the offer's source, once, when it arrives (or when that price does).
    fn settle_prices(self: &Arc<Self>) {
        let _quoting = self.quoting.lock().unwrap();
        let trades: Vec<(PubKey, PubKey, OfferTerms, Deal, Option<f64>)> = {
            let store = self.store.lock().unwrap();
            store
                .state
                .trades
                .iter()
                .filter(|(_, t)| !store.state.blocked.contains(&t.peer))
                .filter_map(|(hex, t)| {
                    let terms = t.offer.clone()?;
                    let deal = Deal::replay(&terms, &t.messages);
                    Some((
                        hex.parse().ok()?,
                        t.peer.parse().ok()?,
                        terms,
                        deal,
                        t.price_check,
                    ))
                })
                .collect()
        };
        for (key, peer, terms, deal, checked) in trades {
            let Ok(expected) = self.trade_price(&terms) else {
                continue;
            };
            match terms.my_role() {
                Role::Maker => {
                    if deal.available(&terms, Role::Maker).contains(&Step::Price) {
                        self.send_action_to(key, Action::Price(expected), true);
                    }
                }
                Role::Taker => {
                    let Some(quoted) = deal.price.filter(|_| checked.is_none()) else {
                        continue;
                    };
                    {
                        let mut store = self.store.lock().unwrap();
                        let mark = store.state.trades.get_mut(&key.to_hex()).map(|t| {
                            t.price_check = Some(expected);
                            trade_mark(
                                &self.me(),
                                &peer,
                                &t.trade_id,
                                MARK_PRICE_CHECK,
                                vec![],
                                &expected.to_string(),
                            )
                        });
                        if let Some(mark) = mark {
                            store.save_event(&mark);
                        }
                    }
                    let off = deal::deviation_pct(quoted, expected);
                    if off > deal::PRICE_TOLERANCE_PCT {
                        self.status(format!(
                            "{}'s price is {off:.1}% off the market, check it",
                            self.name(&peer)
                        ));
                    }
                    self.render_trades();
                }
            }
        }
    }

    /// `key` is the conversation pubkey of the trade
    fn send(self: &Arc<Self>, key: PubKey, text: &str, action: Option<Action>) {
        let Some((peer, trade_id)) = self
            .store
            .lock()
            .unwrap()
            .state
            .trades
            .get(&key.to_hex())
            .and_then(|t| Some((t.peer.parse::<PubKey>().ok()?, t.trade_id.clone())))
        else {
            return;
        };
        if self.is_blocked(&peer) {
            self.status("unblock them first");
            return;
        }
        let conv = Conversation::new(&self.sk(), peer, &trade_id);
        let (relays, needs_ping, evt) = {
            let mut store = self.store.lock().unwrap();
            let Some(t) = store.state.trades.get_mut(&key.to_hex()) else {
                return;
            };
            let offer = t.offer_id.as_deref().and_then(|h| ID::from_hex(h).ok());
            let action = action.map(|a| a.to_tag());
            let (rumor, evt) = conv.wrap(&self.me(), text, offer, action.clone());
            let local = conv.local_event(&rumor, &self.me());
            // until they answer we keep poking them, as they only start listening after a ping
            let needs_ping = !t.messages.iter().any(|m| !m.mine);
            let relays = t.relays.clone();
            t.messages.push(Message {
                id: local.id.to_hex(),
                created_at: rumor.created_at,
                mine: true,
                text: rumor.content,
                action,
                event: evt.to_string(),
                deliveries: vec![],
            });
            store.save_event(&local);
            (relays, needs_ping, (local.id.to_hex(), evt))
        };
        let (msg_id, evt) = evt;
        self.render_trade();
        self.render_trades();

        if needs_ping {
            self.send_ping(key);
        }
        let app = self.clone();
        tokio::spawn(async move {
            if needs_ping {
                // give the other side a moment to open the conversation subscription
                tokio::time::sleep(Duration::from_millis(1500)).await;
            }
            let answers = app
                .deliver(&relays, evt, |answers| {
                    app.set_deliveries(key, &msg_id, answers)
                })
                .await;
            app.store.lock().unwrap().persist();
            if accepted(&answers).is_empty() {
                app.status(format!(
                    "message failed to reach any of {} relays",
                    relays.len()
                ));
            }
            if mostly_failed(&answers) {
                app.show_report(Report::Message(TradeRef::Ongoing(key), msg_id), false);
            }
        });
    }

    /// how the relays took a message of ours so far, kept with it (saved once they all answered)
    fn set_deliveries(&self, peer: PubKey, msg_id: &str, answers: &[Delivery]) {
        {
            let mut store = self.store.lock().unwrap();
            let Some(m) = store
                .state
                .trades
                .get_mut(&peer.to_hex())
                .and_then(|t| t.messages.iter_mut().find(|m| m.id == msg_id))
            else {
                return;
            };
            m.deliveries = answers.to_vec();
        }
        if *self.current_trade.lock().unwrap() == Some(TradeRef::Ongoing(peer)) {
            self.render_trade();
        }
        self.rerender_report(&Report::Message(
            TradeRef::Ongoing(peer),
            msg_id.to_string(),
        ));
    }

    /// review the trade being looked at: it ends if it is ongoing, and a past one can be reviewed
    /// again, whether it was archived or concluded
    fn conclude_trade(self: &Arc<Self>, outcome: Outcome, comment: &str) {
        let current = self.current_trade.lock().unwrap().clone();
        match current {
            Some(TradeRef::Ongoing(_)) => self.end_trade(outcome, comment),
            Some(TradeRef::Past(id)) => {
                let Some(t) = self.store.lock().unwrap().history.get(&id) else {
                    return;
                };
                if let Ok(peer) = t.peer.parse() {
                    self.publish_review(peer, outcome == Outcome::Success, comment, Some(id));
                }
                // looking at the trade, and its status changes when the review is recorded
                let _ = self
                    .ui
                    .upgrade_in_event_loop(|ui| ui.set_trade_concluding(false));
            }
            None => {}
        }
    }

    /// ask the relays we published one review of this past trade to to drop it
    fn delete_review(self: &Arc<Self>, review_id: &str) {
        let Some(TradeRef::Past(id)) = self.current_trade.lock().unwrap().clone() else {
            return;
        };
        let Some((review, peer)) = ({
            let mut store = self.store.lock().unwrap();
            let peer = store
                .history
                .get(&id)
                .and_then(|t| t.peer.parse::<PubKey>().ok());
            let review = store.history.remove_review(&id, review_id);
            if let Some(rid) = review.as_ref().and_then(|r| r.id.parse().ok()) {
                store.forget_event(&rid);
            }
            review.zip(peer)
        }) else {
            return;
        };
        self.render_trade();
        self.render_trades();
        let Ok(rid) = review.id.parse() else {
            return;
        };
        let kind = if review.success {
            KIND_TRADE_SUCCESS
        } else {
            KIND_TRADE_FAILURE
        };
        let evt = deletion(&self.sk(), &rid, kind);
        let app = self.clone();
        tokio::spawn(async move {
            let relays = app.review_relays(&peer).await;
            let (ok, total) = app.publish("review deletion", relays, evt).await;
            app.status(format!("review deleted from {}/{total} relays", ok.len()));
        });
    }

    /// put away the trade being looked at without a review: an ongoing one ends, and a concluded
    /// one that was waiting for its review stops waiting
    fn archive_trade(self: &Arc<Self>) {
        let current = self.current_trade.lock().unwrap().clone();
        match current {
            Some(TradeRef::Ongoing(_)) => self.end_trade(Outcome::Archived, ""),
            Some(TradeRef::Past(id)) => {
                self.store.lock().unwrap().history.archive(&id);
                self.leave_trade();
            }
            None => {}
        }
    }

    /// end the ongoing trade: concluded to be reviewed later, with a public review (success or
    /// failure), or just archived. it moves from the json state to the history database and we
    /// stop listening.
    fn end_trade(self: &Arc<Self>, outcome: Outcome, comment: &str) {
        let Some(TradeRef::Ongoing(key)) = self.current_trade.lock().unwrap().clone() else {
            return;
        };
        let (trade_peer, past_id) = {
            let mut store = self.store.lock().unwrap();
            let Some(trade) = store.state.trades.get(&key.to_hex()).cloned() else {
                return;
            };
            let past_id = match store.history.add(&trade, outcome, comment, now().0) {
                Ok(id) => id,
                Err(err) => {
                    drop(store);
                    self.status(format!("failed to save the trade in the history: {err}"));
                    return;
                }
            };
            store.state.trades.remove(&key.to_hex());
            (trade.peer, past_id)
        };
        self.dm_subs.lock().unwrap().remove(&key.to_hex());
        if matches!(outcome, Outcome::Success | Outcome::Failure)
            && let Ok(peer) = trade_peer.parse()
        {
            self.publish_review(peer, outcome == Outcome::Success, comment, Some(past_id));
        }
        self.leave_trade();
    }

    /// forget a trade that is over, concluded or archived, without keeping it anywhere
    fn delete_trade(self: &Arc<Self>) {
        let Some(TradeRef::Past(id)) = self.current_trade.lock().unwrap().clone() else {
            return;
        };
        self.store.lock().unwrap().history.delete(&id);
        self.leave_trade();
    }

    fn leave_trade(self: &Arc<Self>) {
        *self.current_trade.lock().unwrap() = None;
        self.render_trades();
        let _ = self.ui.upgrade_in_event_loop(|ui| ui.set_page(3));
    }

    /// while we have a real offer open or a real trade going, the app is kept running in the
    /// background so we hear about replies, and it is let go of when there are none
    fn sync_background(&self) {
        if self.offline {
            return;
        }
        let offers = self
            .open_offers()
            .iter()
            .any(|o| !xfk::is_xfk(&o.currency));
        let wanted = offers
            || self
                .store
                .lock()
                .unwrap()
                .state
                .trades
                .values()
                .any(|t| t.offer.as_ref().is_none_or(|o| !xfk::is_xfk(&o.currency)));
        crate::notify::set_background(wanted);
    }

    /// the trades tab: our open offers, ongoing trades and the most recent past ones
    fn render_trades(&self) {
        let open: Vec<OfferItem> = self
            .open_offers()
            .iter()
            .map(|o| self.offer_item(o))
            .collect();
        let (ongoing, pending, past, concluded) = {
            let store = self.store.lock().unwrap();
            let s = &store.state;
            let mut trades: Vec<_> = s.trades.iter().collect();
            trades.sort_by_key(|(_, t)| {
                std::cmp::Reverse(
                    t.messages
                        .last()
                        .map(|m| m.created_at)
                        .unwrap_or(t.started_at),
                )
            });
            let ongoing: Vec<TradeItem> = trades
                .into_iter()
                .filter_map(|(hex, t)| {
                    let pk: PubKey = t.peer.parse().ok()?;
                    let key: PubKey = hex.parse().ok()?;
                    Some(TradeItem {
                        key: key.to_npub().into(),
                        peer_name: display_name(&s.labels, &pk).into(),
                        peer_color: key_color(&pk),
                        summary: t.offer_summary.trim_start_matches("maker ").into(),
                        status: trade_status(t).into(),
                        last: t
                            .messages
                            .last()
                            .zip(shown_messages(t.offer.as_ref(), &t.messages).pop())
                            .map(|(m, shown)| {
                                format!("{}{}", if m.mine { "you: " } else { "" }, shown.text)
                            })
                            .unwrap_or_default()
                            .into(),
                        blocked: s.blocked.contains(&t.peer),
                    })
                })
                .collect();
            let labels = s.labels.clone();
            let past_item = |t: &PastTrade| {
                let pk: PubKey = t.peer.parse().ok()?;
                Some(TradeItem {
                    key: t.trade_id.clone().into(),
                    peer_name: display_name(&labels, &pk).into(),
                    peer_color: key_color(&pk),
                    summary: t.offer_summary.trim_start_matches("maker ").into(),
                    status: past_status(t).into(),
                    last: ago(Timestamp(t.ended_at)).into(),
                    blocked: false,
                })
            };
            // concluded ones stay here until reviewed or archived, however many they are
            let (waiting, rest): (Vec<_>, Vec<_>) = store
                .history
                .list(usize::MAX)
                .into_iter()
                .partition(|t| t.outcome == Outcome::Concluded);
            let pending: Vec<TradeItem> = waiting.iter().filter_map(past_item).collect();
            let past: Vec<TradeItem> = rest
                .iter()
                .take(PAST_TRADES_SHOWN)
                .filter_map(past_item)
                .collect();
            // archived ones weren't concluded
            let concluded = store
                .history
                .any(|t| t.outcome != Outcome::Archived && real_trade(t.offer.as_ref()));
            (ongoing, pending, past, concluded)
        };
        // the tip stays until a real trade went all the way, practice doesn't count
        let newcomer = !concluded;
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_newcomer(newcomer);
            ui.set_my_offers(ModelRc::new(VecModel::from(open)));
            ui.set_ongoing_trades(ModelRc::new(VecModel::from(ongoing)));
            ui.set_pending_trades(ModelRc::new(VecModel::from(pending)));
            ui.set_past_trades(ModelRc::new(VecModel::from(past)));
        });
        self.sync_background();
    }

    fn set_blocked(self: &Arc<Self>, peer: PubKey, blocked: bool) {
        {
            let mut store = self.store.lock().unwrap();
            if blocked {
                store.state.blocked.insert(peer.to_hex());
            } else {
                store.state.blocked.remove(&peer.to_hex());
            }
            store.persist();
        }
        if blocked {
            // stop looking for their messages, that's all a block is
            for key in self.trades_with(&peer) {
                self.dm_subs.lock().unwrap().remove(&key.to_hex());
            }
            self.offers.lock().unwrap().retain(|o| o.maker != peer);
            self.trusted_offers
                .lock()
                .unwrap()
                .retain(|o| o.maker != peer);
            self.render_offers();
        } else {
            for key in self.trades_with(&peer) {
                self.ensure_dm_sub(key, false);
            }
        }
        self.render_trades();
        self.render_trade();
        let _ = self
            .ui
            .upgrade_in_event_loop(move |ui| ui.set_profile_blocked(blocked));
    }

    // ------------------------------------------------------------------------------ labels

    /// labels live only in the local state, they are never published
    fn set_label(self: &Arc<Self>, peer: PubKey, label: &str) {
        if peer == self.me() {
            return;
        }
        {
            let mut store = self.store.lock().unwrap();
            if label.is_empty() {
                store.state.labels.remove(&peer.to_hex());
            } else {
                store.state.labels.insert(peer.to_hex(), label.to_string());
            }
            store.persist();
        }
        self.render_offers();
        self.render_trades();
        self.render_trade();
        self.render_people();
        self.render_profile(peer, false);
        let label = label.to_string();
        let name = self.name(&peer);
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_profile_label(label.into());
            ui.set_profile_name(name.clone().into());
            // the offer page keeps a copy of the offer, so the back button would show the old label
            let mut offer = ui.get_current_offer();
            if offer.maker.as_str() == peer.to_npub() {
                offer.maker_name = name.into();
                ui.set_current_offer(offer);
            }
        });
    }

    fn render_people(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let query = ui.get_people_filter().trim().to_lowercase();
        let mut people: Vec<PersonItem> = self
            .store
            .lock()
            .unwrap()
            .state
            .labels
            .iter()
            .filter_map(|(hex, label)| {
                let pk: PubKey = hex.parse().ok()?;
                let npub = pk.to_npub();
                (query.is_empty()
                    || label.to_lowercase().contains(&query)
                    || npub.contains(&query)
                    || hex.contains(&query))
                .then(|| PersonItem {
                    npub: npub.into(),
                    short: short_npub(&pk).into(),
                    label: label.clone().into(),
                    color: key_color(&pk),
                })
            })
            .collect();
        people.sort_by_key(|p| p.label.to_lowercase());
        ui.set_people(ModelRc::new(VecModel::from(people)));
    }

    // ------------------------------------------------------------------------------ reputation

    fn open_profile(self: &Arc<Self>, who: PubKey) {
        *self.current_profile.lock().unwrap() = Some(who);
        let blocked = self.is_blocked(&who);
        let is_me = who == self.me();
        let label = self.label(&who);
        let name = self.name(&who);
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            if ui.get_page() != 5 {
                ui.set_back_page(ui.get_page());
            }
            ui.set_profile_npub(who.to_npub().into());
            ui.set_profile_name(name.into());
            ui.set_profile_color(key_color(&who));
            ui.set_profile_blocked(blocked);
            ui.set_profile_label(label.into());
            ui.set_profile_is_me(is_me);
            ui.set_own_reviews(ModelRc::default());
            ui.set_others_reviews(ModelRc::default());
            ui.set_others_note("".into());
            ui.set_page(5);
        });
        self.render_profile(who, true);
    }

    /// our own reviews about them first. if those are few, what our most frequent successful
    /// trade partners have said about them.
    fn render_profile(self: &Arc<Self>, who: PubKey, ask_relays: bool) {
        let is_me = who == self.me();
        let (own, trusted, others, labels) = {
            let store = self.store.lock().unwrap();
            let labels = store.state.labels.clone();
            if is_me {
                (store.reviews_by(&self.me()), vec![], vec![], labels)
            } else {
                let own = store.reviews_about(&[self.me()], &who);
                let trusted: Vec<PubKey> = if own.len() < ENOUGH_OWN_REVIEWS {
                    store
                        .top_trade_partners(&self.me(), TRUSTED_PEERS_TO_ASK + 1, &self.xfk_people)
                        .into_iter()
                        .filter(|pk| *pk != who)
                        .take(TRUSTED_PEERS_TO_ASK)
                        .collect()
                } else {
                    vec![]
                };
                let others = if trusted.is_empty() {
                    vec![]
                } else {
                    store.reviews_about(&trusted, &who)
                };
                (own, trusted, others, labels)
            }
        };

        let note = if is_me {
            String::new()
        } else if own.len() >= ENOUGH_OWN_REVIEWS {
            format!("you have {} reviews of your own, that's enough.", own.len())
        } else if trusted.is_empty() {
            "you haven't reviewed any successful trades yet, so there is nobody you trust to ask."
                .into()
        } else if ask_relays {
            format!(
                "asking the {} people you traded with the most…",
                trusted.len()
            )
        } else if others.is_empty() {
            format!(
                "none of the {} people you traded with the most said anything about them.",
                trusted.len()
            )
        } else {
            format!(
                "from the {} people you traded with the most:",
                trusted.len()
            )
        };

        let own_items: Vec<ReviewItem> =
            own.iter().map(|e| review_item(&labels, e, is_me)).collect();
        let other_items: Vec<ReviewItem> = others
            .iter()
            .map(|e| review_item(&labels, e, false))
            .collect();

        let app = self.clone();
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            if *app.current_profile.lock().unwrap() != Some(who) {
                return;
            }
            ui.set_own_reviews(ModelRc::new(VecModel::from(own_items)));
            ui.set_others_reviews(ModelRc::new(VecModel::from(other_items)));
            ui.set_others_note(note.into());
        });

        if ask_relays && !trusted.is_empty() {
            let app = self.clone();
            tokio::spawn(async move {
                let events = app
                    .fetch(
                        app.app_relays(),
                        Filter {
                            kinds: Some(vec![KIND_TRADE_FAILURE, KIND_TRADE_SUCCESS]),
                            authors: Some(trusted),
                            tags: Some(vec![TagQuery("p".into(), vec![who.to_hex()])]),
                            limit: Some(200),
                            ..Default::default()
                        },
                    )
                    .await;
                {
                    let store = app.store.lock().unwrap();
                    for evt in events.iter().filter(|e| e.verify_signature()) {
                        store.save_event(evt);
                    }
                }
                app.render_profile(who, false);
            });
        }
    }

    fn review_trade(self: &Arc<Self>, success: bool, comment: &str) {
        let Some(peer) = *self.current_profile.lock().unwrap() else {
            return;
        };
        self.publish_review(peer, success, comment, None);
        self.render_profile(peer, false);
    }

    /// publish a review about `peer` to our discovery relays, their read relays and our write
    /// relays. if it is about a past trade, `past` is remembered along with where it went, to
    /// delete it from there later.
    fn publish_review(
        self: &Arc<Self>,
        peer: PubKey,
        success: bool,
        comment: &str,
        past: Option<String>,
    ) {
        if peer == self.me() {
            return;
        }
        let evt = trade_review(&self.sk(), &peer, past.as_deref(), success, comment);
        {
            let mut store = self.store.lock().unwrap();
            store.save_event(&evt);
            if let Some(past) = &past {
                store.history.add_review(
                    past,
                    PublishedReview {
                        id: evt.id.to_hex(),
                        success,
                        comment: comment.to_string(),
                        created_at: evt.created_at.0,
                    },
                );
            }
        }
        if let Some(past) = past {
            self.render_trades();
            if *self.current_trade.lock().unwrap() == Some(TradeRef::Past(past)) {
                self.render_trade();
            }
        }

        let app = self.clone();
        tokio::spawn(async move {
            let relays = app.review_relays(&peer).await;
            let (ok, total) = app.publish("trade review", relays, evt).await;
            app.status(format!(
                "trade review published to {}/{total} relays",
                ok.len()
            ));
        });
    }

    /// where our reviews about `peer` go: our own relays, theirs and the ones for finding people.
    /// what we say about the bots stays in the practice market.
    async fn review_relays(self: &Arc<Self>, peer: &PubKey) -> Vec<String> {
        if self.xfk_people.contains(peer) {
            return vec![xfk::RELAY.into()];
        }
        let theirs = self.peer_read_relays(peer).await;
        let ours = self.my_write_relays(false).await;
        merge_relays(&merge_relays(&self.app_relays(), &theirs), &ours)
    }

    /// the relays someone reads from (nip-65), none if we can't find their relay list
    async fn peer_read_relays(&self, peer: &PubKey) -> Vec<String> {
        let indexers: Vec<String> = INDEXER_RELAYS.iter().map(|r| r.to_string()).collect();
        let lists = self
            .fetch(
                indexers,
                Filter {
                    kinds: Some(vec![KIND_RELAY_LIST]),
                    authors: Some(vec![*peer]),
                    ..Default::default()
                },
            )
            .await;
        lists
            .iter()
            .filter(|e| e.pubkey == *peer && e.verify_signature())
            .max_by_key(|e| e.created_at)
            .map(read_relays)
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------------- helpers

fn review_item(labels: &BTreeMap<String, String>, e: &Event, show_subject: bool) -> ReviewItem {
    let subject = show_subject
        .then(|| e.tags.find("p").and_then(|t| t[1].parse::<PubKey>().ok()))
        .flatten();
    ReviewItem {
        id: e.id.to_hex().into(),
        author_name: display_name(labels, &e.pubkey).into(),
        author_color: key_color(&e.pubkey),
        subject_name: subject
            .map(|pk| display_name(labels, &pk))
            .unwrap_or_default()
            .into(),
        subject_color: subject.map(|pk| key_color(&pk)).unwrap_or_default(),
        success: e.kind == KIND_TRADE_SUCCESS,
        comment: e.content.clone().into(),
        time: ago(e.created_at).into(),
    }
}

/// a trade about an offer in a real currency, not one of the practice market or a chat outside
/// of an offer
fn real_trade(terms: Option<&OfferTerms>) -> bool {
    terms.is_some_and(|t| !xfk::is_xfk(&t.currency))
}

/// the currency of the trade we were last in: an ongoing one, by its last message, or one that
/// is over, by when it ended. trades outside of an offer have none. "all" when the market can't
/// filter by it.
fn last_trade_currency(store: &Store) -> Option<String> {
    let ongoing = store
        .state
        .trades
        .values()
        .filter(|t| t.offer.is_some())
        .map(|t| {
            let at = t.messages.last().map_or(t.started_at, |m| m.created_at);
            (at, t.offer.as_ref())
        })
        .max_by_key(|(at, _)| *at);
    let past = store
        .history
        .latest(|t| t.offer.is_some())
        .map(|t| (t.ended_at, t.offer));
    let currency = match (ongoing, past) {
        (Some((at, terms)), Some((ended, _))) if at >= ended => terms?.currency.clone(),
        (_, Some((_, terms))) => terms?.currency,
        (Some((_, terms)), None) => terms?.currency.clone(),
        (None, None) => return None,
    };
    if prices::currency_codes().contains(&currency.as_str()) {
        Some(currency)
    } else {
        Some("all".into())
    }
}

/// how far before a trade started its messages are still taken as part of it. none in the
/// practice market, where everyone is on our clock, and a trade often follows another right away.
/// the note to ourselves about the offer a trade is about. the offer may be gone from the relays
/// by the time we look at the trade again, so the mark has all we need of it.
fn offer_mark_event(
    me: &PubKey,
    conv: &Conversation,
    offer: &Offer,
    terms: &OfferTerms,
    t: &Trade,
) -> Event {
    let content = OfferMark {
        summary: offer_summary(offer),
        terms: terms.clone(),
        relays: t.relays.clone(),
    };
    trade_mark(
        me,
        &conv.peer,
        &conv.trade,
        MARK_OFFER,
        vec![vec!["e".into(), offer.id.to_hex()]],
        &serde_json::to_string(&content).unwrap_or_default(),
    )
}

/// where a trade that has no offer behind it talks: the practice market with the bots, our relays
/// in a chat we started, and where we listen for pings when someone pinged us
fn trade_relays(state: &State, t: &Trade, xfk_people: &[PubKey]) -> Vec<String> {
    if t.peer
        .parse::<PubKey>()
        .is_ok_and(|peer| xfk_people.contains(&peer))
    {
        return vec![xfk::RELAY.into()];
    }
    let ping_relays: Vec<String> = state
        .ping_relays
        .iter()
        .filter(|r| *r != xfk::RELAY)
        .cloned()
        .collect();
    if t.started_by_me || ping_relays.is_empty() {
        state.app_relays.clone()
    } else {
        ping_relays
    }
}

fn start_slack(t: &Trade) -> u32 {
    if !t.relays.is_empty() && t.relays.iter().all(|r| r == xfk::RELAY) {
        0
    } else {
        TRADE_START_SLACK
    }
}

fn trade_status(t: &Trade) -> String {
    if let Some(terms) = &t.offer {
        let deal = Deal::replay(terms, &t.messages);
        let next = deal.next_step(terms);
        return match deal.price.zip(t.price_check) {
            Some((quoted, expected))
                if deal::deviation_pct(quoted, expected) > deal::PRICE_TOLERANCE_PCT =>
            {
                format!("⚠ their price is off the market · {next}")
            }
            _ => next,
        };
    }
    let from_them = t.messages.iter().any(|m| !m.mine);
    let from_me = t.messages.iter().any(|m| m.mine);
    match t.messages.last() {
        _ if !from_them && (from_me || t.started_by_me) => "waiting for their answer".into(),
        _ if !from_me => "they are waiting for your answer".into(),
        Some(m) if !m.mine => "negotiating · your turn".into(),
        _ => "negotiating · their turn".into(),
    }
}

/// how a message shows in the chat
struct Shown {
    text: String,
    /// a structured one, shown by what it sets and not by the text that came with it
    meta: bool,
    /// a structured one that made no sense when it arrived, so it set nothing
    ignored: bool,
}

/// each message as the chat shows it, in the same order
fn shown_messages(terms: Option<&OfferTerms>, messages: &[Message]) -> Vec<Shown> {
    let mut summaries = vec![].into_iter();
    if let Some(terms) = terms {
        let me = terms.my_role();
        let mut list = vec![];
        let mut i = 0;
        let with_actions: Vec<bool> = messages
            .iter()
            .filter_map(|m| {
                m.action
                    .as_deref()
                    .and_then(Action::from_tag)
                    .map(|_| m.mine)
            })
            .collect();
        Deal::replay_noting(terms, messages, |action, before, applied| {
            let who = if with_actions[i] { me } else { me.other() };
            i += 1;
            list.push((action.summary(terms, who, before), applied));
        });
        summaries = list.into_iter();
    }
    messages
        .iter()
        .map(|m| {
            let structured =
                terms.is_some() && m.action.as_deref().and_then(Action::from_tag).is_some();
            match structured.then(|| summaries.next()).flatten() {
                Some((text, applied)) => Shown {
                    text,
                    meta: true,
                    ignored: !applied,
                },
                None => Shown {
                    text: m.text.clone(),
                    meta: false,
                    ignored: false,
                },
            }
        })
        .collect()
}

fn offer_summary(offer: &Offer) -> String {
    format!(
        "{}s bitcoin for {} at market {} · {} · {}",
        offer.side.as_str(),
        offer.currency,
        prices::format_premium(offer.premium),
        amount_range(offer.amount_min, offer.amount_max, &offer.currency),
        offer.payment_methods.join(", ")
    )
}

/// the deal panel next to the chat: where the trade stands, and the forms for our next steps.
/// `live` is what a bitcoin costs now by the offer's price source and premium, absent for past
/// trades. `check` is what it cost by them when the maker's price arrived, for the taker.
fn deal_view(
    terms: &OfferTerms,
    deal: &Deal,
    live: Option<Result<f64, String>>,
    check: Option<f64>,
) -> DealView {
    let ongoing = live.is_some();
    let live_price = live.as_ref().and_then(|p| p.as_ref().ok().copied());
    let source = format!(
        "{} {}",
        terms.price_api,
        prices::format_premium(terms.premium)
    );
    let me = terms.my_role();
    let cur = &terms.currency;
    let i_get_bitcoin = me == terms.bitcoin_receiver();
    let who = |role: Role| if role == me { "you" } else { "they" };
    let sats = |n: u64| format!("{} sats", thousands(n as f64));
    let to_whom = |receiver: Role| if receiver == me { "you" } else { "them" };

    // the maker's price is the one that counts. the taker sees how it compares to the source.
    let mut price_warning = String::new();
    let price = match (deal.price, me) {
        (None, _) => None,
        (Some(p), Role::Maker) => Some(format!("{} {cur}/BTC, as you said", thousands(p))),
        (Some(p), Role::Taker) => Some({
            let quoted = format!("{} {cur}/BTC, as they said", thousands(p));
            match (check, &live) {
                (Some(expected), _) => {
                    let off = deal::deviation_pct(p, expected);
                    if off > deal::PRICE_TOLERANCE_PCT {
                        price_warning = format!(
                            "Their price is {off:.1}% off {source}, which said {} {cur}/BTC when \
                             it arrived. Ask them about it before going on.",
                            thousands(expected)
                        );
                    }
                    format!("{quoted} · {off:.1}% off {source}")
                }
                (None, Some(Err(why))) => format!("{quoted} · couldn't check it: {why}"),
                _ => quoted,
            }
        }),
    };

    // only what is set, the pairs side by side
    let pairs: Vec<[Option<(String, String)>; 2]> = vec![
        [
            deal.amount.map(|a| (cur.clone(), fiat(a))),
            deal.sats().map(|n| ("bitcoin".into(), sats(n))),
        ],
        [price.map(|p| ("price".into(), p)), None],
        [
            deal.bitcoin_method
                .clone()
                .map(|m| (format!("bitcoin sent via"), m)),
            deal.payment_method
                .clone()
                .map(|m| (format!("{cur} sent via"), m)),
        ],
        [
            deal.bitcoin_details.clone().map(|d| {
                (
                    format!("bitcoin goes to {}", to_whom(terms.bitcoin_receiver())),
                    d,
                )
            }),
            None,
        ],
        [
            deal.payment_details.clone().map(|d| {
                (
                    format!(
                        "{cur} goes to {}",
                        to_whom(terms.bitcoin_receiver().other())
                    ),
                    d,
                )
            }),
            None,
        ],
        [
            deal.taker_sent
                .then(|| (format!("{} sent first", who(Role::Taker)), "✓ sent".into())),
            deal.maker_sent
                .then(|| (format!("then {} sent", who(Role::Maker)), "✓ sent".into())),
        ],
    ];
    let rows: Vec<DealRow> = pairs
        .into_iter()
        .filter_map(|pair| {
            let mut set = pair.into_iter().flatten();
            let (label, value) = set.next()?;
            let (label2, value2) = set.next().unwrap_or_default();
            Some(DealRow {
                label: label.into(),
                value: value.into(),
                label2: label2.into(),
                value2: value2.into(),
            })
        })
        .collect();

    let mut view = DealView {
        active: true,
        price_warning: price_warning.into(),
        rows: ModelRc::new(VecModel::from(rows)),
        amount_hint: amount_range(terms.amount_min, terms.amount_max, cur).into(),
        sats_per_unit: live_price
            .map(|p| (100_000_000.0 / p) as f32)
            .unwrap_or(0.0),
        ..Default::default()
    };
    if !ongoing {
        return view;
    }
    let strings = |v: &[String]| {
        ModelRc::new(VecModel::from(
            v.iter().map(SharedString::from).collect::<Vec<_>>(),
        ))
    };
    if deal.concluded() {
        view.review_hint = "Both sides say they have sent their part. There is no hurry to review: \
             wait until you have really checked that you got what you were owed. If the payment \
             can be reversed (cards, PayPal, some bank transfers), consider waiting a day or two \
             for a reversal before you rate them, as a review is public. You can conclude the \
             trade now and review it later, or archive it if you never will, and delete a \
             review if you change your mind."
            .into();
    }
    for step in deal.available(terms, me) {
        match step {
            Step::Amount => view.pick_amount = true,
            // it goes out on its own once we know the market price, unless we can't get it
            Step::Price => {
                if let Some(Err(why)) = &live {
                    view.quote_price = true;
                    view.quote_hint =
                        format!("{why}. enter the price of one bitcoin in {cur} yourself.").into();
                }
            }
            Step::BitcoinMethod => view.bitcoin_choices = strings(&terms.bitcoin_methods),
            Step::PaymentMethod => view.payment_choices = strings(&terms.payment_methods),
            Step::BitcoinDetails | Step::PaymentDetails => {
                view.give_details = true;
                let (what, method) = if step == Step::BitcoinDetails {
                    let m = deal.bitcoin_method.clone().unwrap_or_default();
                    (deal::destination_name(&m).to_string(), m)
                } else {
                    let m = deal.payment_method.clone().unwrap_or_default();
                    (format!("{cur} payment details"), m)
                };
                view.details_label = format!("your {what}").into();
                let amount = if step == Step::BitcoinDetails {
                    deal.sats().map(sats)
                } else {
                    deal.amount.map(|a| format!("{} {cur}", fiat(a)))
                }
                .unwrap_or_default();
                view.details_hint = if method.is_empty() {
                    format!("where they should send the {amount}.")
                } else {
                    format!("where they should send the {amount} via {method}.")
                }
                .into();
            }
            Step::TakerSends | Step::MakerSends => {
                view.can_send = true;
                let (what, via, to) = if i_get_bitcoin {
                    let a = deal.amount.unwrap_or_default();
                    (
                        format!("{} {cur}", fiat(a)),
                        deal.payment_method.clone(),
                        deal.payment_details.clone(),
                    )
                } else {
                    (
                        sats(deal.sats().unwrap_or_default()),
                        deal.bitcoin_method.clone(),
                        deal.bitcoin_details.clone(),
                    )
                };
                let via = via
                    .filter(|m| !m.is_empty())
                    .map(|m| format!(" via {m}"))
                    .unwrap_or_default();
                view.send_hint = format!("send {what}{via} to:\n{}", to.unwrap_or_default()).into();
                view.send_label = format!("I've sent the {what}").into();
            }
        }
    }
    view
}

fn past_status(t: &PastTrade) -> String {
    let outcome = match t.outcome {
        Outcome::Concluded => "concluded, review pending",
        Outcome::Success => "✓ concluded: success",
        Outcome::Failure => "✗ concluded: failure",
        Outcome::Archived => "archived",
    };
    if t.comment.is_empty() {
        outcome.into()
    } else {
        format!("{outcome} · \"{}\"", t.comment)
    }
}

/// the relays that took it
fn accepted(answers: &[Delivery]) -> Vec<String> {
    answers
        .iter()
        .filter(|(_, a)| matches!(a, Some(Ok(()))))
        .map(|(url, _)| url.clone())
        .collect()
}

/// half or more of the relays we tried didn't take it, counting the ones that never answered
fn mostly_failed(answers: &[Delivery]) -> bool {
    let failed = answers.len() - accepted(answers).len();
    !answers.is_empty() && failed * 2 >= answers.len()
}

/// "sending…" under one of our messages until the first relay answers. the answers themselves
/// are in the relay popup, opened by clicking the message's time
fn delivery_summary(answers: &[Delivery]) -> Option<&'static str> {
    (!answers.is_empty() && answers.iter().all(|(_, a)| a.is_none())).then_some("sending…")
}

/// json indented for reading, or as it is if it doesn't parse
fn pretty_json(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .and_then(|v| serde_json::to_string_pretty(&v))
        .unwrap_or_else(|_| json.to_string())
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// the host (and port) of a relay url, which is also where its website usually is
/// `a` and then whatever of `b` isn't in it already, as normalized urls
fn merge_relays(a: &[String], b: &[String]) -> Vec<String> {
    let mut all: Vec<String> = vec![];
    for r in a.iter().chain(b) {
        let url = ritualistic::normalize_url(r).map_or_else(|_| r.clone(), |u| u.to_string());
        if !all.contains(&url) {
            all.push(url);
        }
    }
    all
}

fn relay_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest)
}

fn display_name(labels: &BTreeMap<String, String>, pk: &PubKey) -> String {
    labels
        .get(&pk.to_hex())
        .cloned()
        .unwrap_or_else(|| short_npub(pk))
}

/// a color of their own for someone, so the same key always looks the same. picked by hue in
/// oklch, where every hue at the same lightness reads about as well on a light background.
fn key_color(pk: &PubKey) -> slint::Color {
    let hex = pk.to_hex();
    let seed = u32::from_str_radix(&hex[..8], 16).unwrap_or_default();
    let hue = (seed % 360) as f32;
    oklch(0.52, 0.14, hue)
}

fn oklch(l: f32, c: f32, hue: f32) -> slint::Color {
    let (a, b) = (c * hue.to_radians().cos(), c * hue.to_radians().sin());
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let linear = [
        4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_,
        -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_,
        -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_,
    ];
    let [r, g, b] = linear.map(|v| {
        let v = v.clamp(0.0, 1.0);
        let srgb = if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (srgb * 255.0).round() as u8
    });
    slint::Color::from_rgb_u8(r, g, b)
}

fn short_npub(pk: &PubKey) -> String {
    let npub = pk.to_npub();
    format!("{}…{}", &npub[..12], &npub[npub.len() - 6..])
}

fn ago(ts: Timestamp) -> String {
    let secs = now().0.saturating_sub(ts.0);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

/// "20–100 USD", or "50 USD" for a fixed amount
fn amount_range(min: f64, max: f64, currency: &str) -> String {
    if fiat(min) == fiat(max) {
        format!("{} {currency}", fiat(min))
    } else {
        format!("{}–{} {currency}", fiat(min), fiat(max))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn thousands() {
        assert_eq!(super::thousands(1234567.4), "1,234,567");
        assert_eq!(super::thousands(999.0), "999");
    }

    #[test]
    fn merge_relays() {
        let a = vec!["wss://nostr.wine".to_string()];
        let b = vec!["wss://nos.lol".to_string(), "wss://nostr.wine/".to_string()];
        assert_eq!(
            super::merge_relays(&a, &b),
            vec!["wss://nostr.wine/", "wss://nos.lol/"]
        );
    }

    #[test]
    fn amount_range() {
        assert_eq!(super::amount_range(20.0, 1500.5, "USD"), "20–1,500.50 USD");
        assert_eq!(super::amount_range(50.0, 50.0, "MXN"), "50 MXN");
    }
}

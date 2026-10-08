//! glue between the ui, the relays and the local store

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ritualistic::{
    Event, Filter, ID, Network, PubKey, SecretKey, SubscriptionOptions, TagQuery, Timestamp,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::form::{self, OfferTemplate};
use crate::history::{Outcome, PastTrade};
use crate::prices::{self, PriceApi};
use crate::protocol::*;
use crate::store::{ENOUGH_OWN_REPORTS, Message, MyOffer, Store, TRUSTED_PEERS_TO_ASK, Trade};
use crate::{
    AppWindow, MsgItem, OfferItem, PayItem, PersonItem, RelayItem, ReportItem, TradeItem, TradeView,
};

const FETCH_TIMEOUT: Duration = Duration::from_secs(12);
const RECONNECT_DELAY: Duration = Duration::from_secs(10);
const PRICE_REFRESH: Duration = Duration::from_secs(60);
/// how far before a trade started its messages are still taken as part of it, for clock skew
const TRADE_START_SLACK: u32 = 5 * 60;
/// how many past trades the trades tab lists
const PAST_TRADES_SHOWN: usize = 30;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TradeRef {
    /// keyed by the peer, in the json state
    Ongoing(PubKey),
    /// keyed by its row in the history database
    Past(i64),
}

struct App {
    sk: SecretKey,
    me: PubKey,
    net: Network,
    rt: tokio::runtime::Handle,
    ui: slint::Weak<AppWindow>,
    store: Mutex<Store>,

    offers: Mutex<Vec<Offer>>,
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
    /// peer hex -> subscription to the conversation pubkey
    dm_subs: Mutex<HashMap<String, LiveSub>>,

    current_trade: Mutex<Option<TradeRef>>,
    current_profile: Mutex<Option<PubKey>>,
}

pub fn run(data_dir: PathBuf) -> Result<(), slint::PlatformError> {
    let _ = std::fs::create_dir_all(&data_dir);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start tokio");
    // ritualistic spawns tasks (even on drop), so the runtime must be reachable from the ui thread
    let _guard = rt.enter();

    let sk = crate::store::load_or_create_key(&data_dir);
    let me = sk.pubkey();
    let ui = AppWindow::new()?;

    let app = Arc::new(App {
        sk,
        me,
        net: Network::new(),
        rt: rt.handle().clone(),
        ui: ui.as_weak(),
        store: Mutex::new(Store::open(&data_dir)),
        offers: Mutex::new(vec![]),
        prices: Mutex::new(HashMap::new()),
        price_errors: Mutex::new(HashMap::new()),
        form_pair: Mutex::new(Default::default()),
        locale_currency: form::locale_currency(),
        filters: Mutex::new(("all".into(), "all".into())),
        ping_sub: Mutex::new(None),
        dm_subs: Mutex::new(HashMap::new()),
        current_trade: Mutex::new(None),
        current_profile: Mutex::new(None),
    });

    setup_ui(&app, &ui);

    app.restart_ping_sub();
    let peers: Vec<String> = app
        .store
        .lock()
        .unwrap()
        .state
        .trades
        .keys()
        .cloned()
        .collect();
    for peer in peers {
        if let Ok(pk) = peer.parse() {
            app.ensure_dm_sub(pk, false);
        }
    }
    app.refresh_offers();

    let a = app.clone();
    rt.spawn(async move {
        loop {
            a.fetch_prices().await;
            tokio::time::sleep(PRICE_REFRESH).await;
        }
    });

    ui.run()
}

fn setup_ui(app: &Arc<App>, ui: &AppWindow) {
    ui.set_npub(app.me.to_npub().into());

    let mut currencies: Vec<SharedString> = vec!["all".into()];
    currencies.extend(prices::currency_codes().into_iter().map(SharedString::from));
    ui.set_create_currencies(ModelRc::new(VecModel::from(currencies[1..].to_vec())));
    ui.set_currencies(ModelRc::new(VecModel::from(currencies)));
    // the combobox drops a value that isn't in its model yet, so set it again now that it is
    ui.set_filter_currency("all".into());

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
        let item = {
            let offers = a.offers.lock().unwrap();
            let Some(offer) = offers.iter().find(|o| o.id.to_hex() == id.as_str()) else {
                return;
            };
            a.offer_item(offer)
        };
        if let Some(ui) = a.ui.upgrade() {
            ui.set_current_offer(item);
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

    let a = app.clone();
    ui.on_publish_offer(move || a.publish_offer());

    let a = app.clone();
    ui.on_open_trade(move |peer| {
        if let Ok(pk) = peer.parse::<PubKey>() {
            a.open_trade(pk);
        }
    });

    let a = app.clone();
    ui.on_open_past_trade(move |id| {
        if let Ok(id) = id.parse::<i64>() {
            a.show_trade(TradeRef::Past(id));
        }
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
        a.end_trade(outcome, comment.trim());
    });

    let a = app.clone();
    ui.on_archive_trade(move || a.end_trade(Outcome::Archived, ""));

    let a = app.clone();
    ui.on_delete_trade(move || a.delete_trade());

    let a = app.clone();
    ui.on_send_message(move |text| a.send_message(text.trim()));

    let a = app.clone();
    ui.on_open_profile(move |who| {
        if let Ok(pk) = who.parse::<PubKey>() {
            a.open_profile(pk);
        }
    });

    let a = app.clone();
    ui.on_report_trade(move |success, comment| a.report_trade(success, comment.trim()));

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
    ui.on_toggle_relay(move |url, enabled| {
        {
            let mut store = a.store.lock().unwrap();
            let relays = &mut store.state.app_relays;
            relays.retain(|r| r != url.as_str());
            if enabled {
                relays.push(url.to_string());
            }
            store.persist();
        }
        a.relays_changed();
    });

    let a = app.clone();
    ui.on_remove_relay(move |url| {
        {
            let mut store = a.store.lock().unwrap();
            store.state.app_relays.retain(|r| r != url.as_str());
            store.state.known_relays.retain(|r| r != url.as_str());
            store.persist();
        }
        a.relays_changed();
    });

    let a = app.clone();
    ui.on_add_relay(move |url| {
        let Ok(url) = ritualistic::normalize_url(url.trim()) else {
            a.status("invalid relay url");
            return;
        };
        let url = url.to_string();
        {
            let mut store = a.store.lock().unwrap();
            if !store.state.known_relays.contains(&url) {
                store.state.known_relays.push(url.clone());
            }
            if !store.state.app_relays.contains(&url) {
                store.state.app_relays.push(url);
            }
            store.persist();
        }
        a.relays_changed();
    });

    let a = app.clone();
    ui.on_reveal_nsec(move || {
        if let Some(ui) = a.ui.upgrade() {
            if ui.get_nsec().is_empty() {
                ui.set_nsec(a.sk.to_nsec().into());
            } else {
                ui.set_nsec("".into());
            }
        }
    });
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

fn checked_names(model: &ModelRc<PayItem>) -> Vec<String> {
    model
        .iter()
        .filter(|p| p.checked)
        .map(|p| p.name.to_string())
        .collect()
}

impl App {
    fn status(&self, msg: impl Into<String>) {
        let msg: String = msg.into();
        log::info!("{msg}");
        let _ = self
            .ui
            .upgrade_in_event_loop(move |ui| ui.set_status(msg.into()));
    }

    fn app_relays(&self) -> Vec<String> {
        self.store.lock().unwrap().state.app_relays.clone()
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
        if relays.is_empty() {
            return events;
        }
        let mut sub = self
            .net
            .subscribe(&relays, filter, SubscriptionOptions::default())
            .await;
        let deadline = tokio::time::sleep(FETCH_TIMEOUT);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                occ = sub.recv() => match occ {
                    Some(ritualistic::Occurrence::Event(evt, _)) => events.push(*evt),
                    _ => break,
                },
                _ = &mut deadline => break,
            }
        }
        events
    }

    /// returns the relays that accepted it, and how many we tried
    async fn publish(&self, relays: Vec<String>, evt: Event) -> (Vec<String>, usize) {
        let total = relays.len();
        let mut results = self.net.clone().publish_many(&relays, evt).await;
        let mut ok = vec![];
        while let Some(res) = results.recv().await {
            match res.error {
                None => ok.push(res.relay_url.to_string()),
                Some(err) => log::warn!("publish to {} failed: {err}", res.relay_url),
            }
        }
        (ok, total)
    }

    /// keep a filter open on each relay, reconnecting forever, calling `on_event` once per event
    fn live(
        self: &Arc<Self>,
        relays: &[String],
        filter: Filter,
        on_event: impl Fn(&Arc<App>, Event) + Send + Sync + 'static,
    ) -> LiveSub {
        let on_event = Arc::new(on_event);
        let seen = Arc::new(Mutex::new(HashSet::<ID>::new()));
        let handles = relays
            .iter()
            .map(|url| {
                let (app, url, filter) = (self.clone(), url.clone(), filter.clone());
                let (on_event, seen) = (on_event.clone(), seen.clone());
                self.rt
                    .spawn(async move {
                        loop {
                            let mut sub = app
                                .net
                                .subscribe([&url], filter.clone(), SubscriptionOptions::default())
                                .await;
                            while let Some(occ) = sub.recv().await {
                                if let ritualistic::Occurrence::Event(evt, _) = occ
                                    && seen.lock().unwrap().insert(evt.id)
                                {
                                    on_event(&app, *evt);
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
        self.render_relays();
        self.restart_ping_sub();
        self.refresh_offers();
    }

    fn render_relays(&self) {
        let (known, enabled, no_history) = {
            let s = &self.store.lock().unwrap().state;
            let mut known = s.known_relays.clone();
            for r in &s.app_relays {
                if !known.contains(r) {
                    known.push(r.clone());
                }
            }
            (known, s.app_relays.clone(), s.recent_offers.is_empty())
        };
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            let items: Vec<RelayItem> = known
                .iter()
                .map(|url| RelayItem {
                    url: url.into(),
                    enabled: enabled.contains(url),
                })
                .collect();
            ui.set_relays(ModelRc::new(VecModel::from(items)));
            // once we made offers the form takes its relays from them instead
            if no_history {
                ui.set_create_relays(enabled.join(", ").into());
            }
        });
    }

    // ------------------------------------------------------------------------------ market

    fn refresh_offers(self: &Arc<Self>) {
        let app = self.clone();
        self.rt.spawn(async move {
            let _ = app.ui.upgrade_in_event_loop(|ui| ui.set_loading(true));
            let relays = app.app_relays();
            let since = Timestamp(Timestamp::now().0.saturating_sub(OFFER_TTL_SECS));
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

            let mut offers: Vec<Offer> = events
                .iter()
                .filter(|e| e.created_at >= since && e.verify_signature())
                .filter_map(Offer::from_event)
                .filter(|o| !app.is_blocked(&o.maker))
                .collect();
            // our own offers only show while we still have them open, a relay may have
            // ignored the deletion of one we closed
            let mine: HashSet<ID> = app.open_offers().iter().map(|o| o.id).collect();
            offers.retain(|o| o.maker != app.me || mine.contains(&o.id));
            offers.sort_by_key(|o| std::cmp::Reverse(o.created_at));
            let count = offers.len();
            *app.offers.lock().unwrap() = offers;
            let _ = app.ui.upgrade_in_event_loop(|ui| ui.set_loading(false));
            app.status(format!("{count} offers from {} relays", relays.len()));
            app.render_offers();
            app.fetch_prices().await;
        });
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
        self.fetch_pairs(pairs).await;
    }

    async fn fetch_pairs(self: &Arc<Self>, pairs: HashSet<(String, String)>) {
        let handles: Vec<_> = pairs
            .into_iter()
            .filter_map(|(cur, api_id)| {
                let api = PriceApi::from_id(&api_id)?;
                Some(self.rt.spawn(async move {
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
            maker_short: short_npub(&o.maker).into(),
            maker_label: self.label(&o.maker).into(),
            mine: o.maker == self.me,
            side: o.side.as_str().into(),
            currency: o.currency.clone().into(),
            price_api: o.price_api.clone().into(),
            premium: prices::format_premium(o.premium).into(),
            price: price.into(),
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

    fn render_offers(&self) {
        let (side, currency) = self.filters.lock().unwrap().clone();
        let items: Vec<OfferItem> = self
            .offers
            .lock()
            .unwrap()
            .iter()
            .filter(|o| side == "all" || o.side.as_str() == side)
            .filter(|o| currency == "all" || currency.is_empty() || o.currency == currency)
            .map(|o| self.offer_item(o))
            .collect();
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_offers(ModelRc::new(VecModel::from(items)));
        });
    }

    // ------------------------------------------------------------------------------ new offer

    /// fill the form with whatever our last offers had in common, or the locale's currency and
    /// the app relays when we never made one
    fn reset_create_form(self: &Arc<Self>, ui: &AppWindow) {
        let (recent, app_relays) = {
            let s = &self.store.lock().unwrap().state;
            (s.recent_offers.clone(), s.app_relays.clone())
        };
        let d = form::defaults(&recent, &self.locale_currency, &app_relays);
        ui.set_create_relays(d.relays.join(", ").into());
        ui.set_create_side(d.side.into());
        ui.set_create_currency(d.currency.clone().into());
        ui.set_create_premium(prices::format_premium(d.premium).into());
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

        self.form_pair_changed(currency.into(), api.into());
    }

    fn form_pair_changed(self: &Arc<Self>, currency: String, api: String) {
        let pair = (currency, api);
        *self.form_pair.lock().unwrap() = pair.clone();
        if !self.prices.lock().unwrap().contains_key(&pair) {
            let app = self.clone();
            self.rt
                .spawn(async move { app.fetch_pairs(HashSet::from([pair])).await });
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
                None => "invalid premium, use something like +1% or -0.5%".to_string(),
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
            self.status("invalid premium, use something like +1% or -0.5%");
            return;
        };
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
        let relays: Vec<String> = split_list(&ui.get_create_relays())
            .iter()
            .filter_map(|r| ritualistic::normalize_url(r).ok())
            .map(|u| u.to_string())
            .collect();
        if relays.is_empty() {
            self.status("add at least one relay for the negotiation");
            return;
        }

        let template = OfferTemplate {
            side: ui.get_create_side().into(),
            currency: ui.get_create_currency().into(),
            price_api: ui.get_create_api().into(),
            premium,
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
            currency: ui.get_create_currency().to_string(),
            price_api: ui.get_create_api().to_string(),
            premium,
            payment_methods,
            bitcoin_methods,
            relays: relays.clone(),
            description: ui.get_create_description().trim().to_string(),
        }
        .to_event(&self.sk);
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
            let recent = &mut store.state.recent_offers;
            recent.insert(0, template);
            recent.truncate(form::RECENT_OFFERS);
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
        self.status("publishing offer…");

        let app = self.clone();
        self.rt.spawn(async move {
            let id = evt.id;
            let (ok, total) = app.publish(app.app_relays(), evt).await;
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

        let relays = if mine.relays.is_empty() {
            self.app_relays()
        } else {
            mine.relays
        };
        let evt = deletion(&self.sk, &mine.event.id, KIND_OFFER);
        let app = self.clone();
        self.rt.spawn(async move {
            let (ok, total) = app.publish(relays, evt).await;
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
            tags: Some(vec![TagQuery("p".into(), vec![self.me.to_hex()])]),
            since: Some(Timestamp::now()),
            ..Default::default()
        };
        let sub = self.live(&relays, filter, |app, evt| app.on_ping(evt));
        *self.ping_sub.lock().unwrap() = Some(sub);
    }

    /// someone wants to trade with us: that starts a trade, unless there is one going on already
    fn on_ping(self: &Arc<Self>, evt: Event) {
        let peer = evt.pubkey;
        if peer == self.me || !evt.verify_signature() || self.is_blocked(&peer) {
            return;
        }
        let is_new = {
            let mut store = self.store.lock().unwrap();
            let is_new = !store.state.trades.contains_key(&peer.to_hex());
            if is_new {
                let relays = if store.state.ping_relays.is_empty() {
                    store.state.app_relays.clone()
                } else {
                    store.state.ping_relays.clone()
                };
                store.state.trades.insert(
                    peer.to_hex(),
                    Trade {
                        relays,
                        offer_summary: "pinged you about your offer".into(),
                        started_at: evt.created_at.0,
                        started_by_me: false,
                        ..Default::default()
                    },
                );
                store.persist();
            }
            is_new
        };
        self.ensure_dm_sub(peer, false);
        if is_new {
            self.status(format!("new trade with {}", self.name(&peer)));
            self.render_trades();
        }
    }

    /// subscribe to `authors:[conversation pubkey]`, which is all it takes to hear from a peer.
    /// blocking someone is just never doing this for them. only ongoing trades are listened to.
    fn ensure_dm_sub(self: &Arc<Self>, peer: PubKey, restart: bool) {
        if self.is_blocked(&peer) {
            return;
        }
        let hex = peer.to_hex();
        let mut subs = self.dm_subs.lock().unwrap();
        if subs.contains_key(&hex) && !restart {
            return;
        }
        let Some((relays, started_at)) = self
            .store
            .lock()
            .unwrap()
            .state
            .trades
            .get(&hex)
            .map(|t| (t.relays.clone(), t.started_at))
        else {
            return;
        };
        let conv = Conversation::new(&self.sk, peer);
        let filter = Filter {
            kinds: Some(vec![KIND_DM]),
            authors: Some(vec![conv.pubkey()]),
            // dms stay on relays for a while, older ones belong to an earlier trade
            since: (started_at > 0)
                .then(|| Timestamp(started_at.saturating_sub(TRADE_START_SLACK))),
            ..Default::default()
        };
        let sub = self.live(&relays, filter, move |app, evt| app.on_dm(&conv, evt));
        subs.insert(hex, sub);
    }

    fn on_dm(self: &Arc<Self>, conv: &Conversation, evt: Event) {
        let Some(rumor) = conv.unwrap(&evt) else {
            return;
        };
        {
            let mut store = self.store.lock().unwrap();
            // whoever signed this could have been either of us: only our local memory of the rumors
            // we created tells us which ones are ours
            if store.state.my_rumors.contains(&rumor.id) {
                return;
            }
            let Some(t) = store.state.trades.get_mut(&conv.peer.to_hex()) else {
                return;
            };
            if t.messages.iter().any(|m| m.id == rumor.id)
                || rumor.created_at < t.started_at.saturating_sub(TRADE_START_SLACK)
            {
                return;
            }
            t.messages.push(Message {
                id: rumor.id,
                created_at: rumor.created_at,
                mine: false,
                text: rumor.content,
            });
            t.messages.sort_by_key(|m| m.created_at);
            store.persist();
        }
        self.render_trades();
        if *self.current_trade.lock().unwrap() == Some(TradeRef::Ongoing(conv.peer)) {
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
        if offer.maker == self.me {
            return;
        }
        if self.is_blocked(&offer.maker) {
            self.status("you blocked this maker");
            return;
        }
        {
            let mut store = self.store.lock().unwrap();
            let t = store
                .state
                .trades
                .entry(offer.maker.to_hex())
                .or_insert_with(|| Trade {
                    started_at: Timestamp::now().0,
                    started_by_me: true,
                    ..Default::default()
                });
            t.relays = offer.relays.clone();
            t.offer_id = Some(offer.id.to_hex());
            t.offer_summary = format!(
                "maker {}s bitcoin for {} at market {} · {}",
                offer.side.as_str(),
                offer.currency,
                prices::format_premium(offer.premium),
                offer.payment_methods.join(", ")
            );
            store.persist();
        }
        self.ensure_dm_sub(offer.maker, true);
        self.send_ping(offer.maker);
        self.render_trades();
        self.open_trade(offer.maker);
    }

    fn send_ping(self: &Arc<Self>, peer: PubKey) {
        let Some(relays) = self.trade_relays(&peer) else {
            return;
        };
        let evt = ping(&self.sk, &peer);
        let app = self.clone();
        self.rt.spawn(async move {
            app.publish(relays, evt).await;
        });
    }

    fn trade_relays(&self, peer: &PubKey) -> Option<Vec<String>> {
        self.store
            .lock()
            .unwrap()
            .state
            .trades
            .get(&peer.to_hex())
            .map(|t| t.relays.clone())
    }

    /// open the ongoing trade with someone, starting one if there is none
    fn open_trade(self: &Arc<Self>, peer: PubKey) {
        if peer == self.me {
            return;
        }
        let created = {
            let mut store = self.store.lock().unwrap();
            let exists = store.state.trades.contains_key(&peer.to_hex());
            if !exists {
                // talking to someone outside of an offer, use our own relays
                let relays = store.state.app_relays.clone();
                store.state.trades.insert(
                    peer.to_hex(),
                    Trade {
                        relays,
                        offer_summary: "direct chat".into(),
                        started_at: Timestamp::now().0,
                        started_by_me: true,
                        ..Default::default()
                    },
                );
                store.persist();
            }
            !exists
        };
        if created {
            self.ensure_dm_sub(peer, false);
            self.send_ping(peer);
            self.render_trades();
        }
        self.show_trade(TradeRef::Ongoing(peer));
    }

    fn show_trade(self: &Arc<Self>, which: TradeRef) {
        *self.current_trade.lock().unwrap() = Some(which);
        self.render_trade();
        let _ = self.ui.upgrade_in_event_loop(|ui| {
            ui.set_trade_confirm_delete(false);
            ui.set_trade_concluding(false);
            ui.set_back_page(3);
            ui.set_page(4);
        });
    }

    fn render_trade(&self) {
        let Some(which) = *self.current_trade.lock().unwrap() else {
            return;
        };
        let store = self.store.lock().unwrap();
        let (peer, view) = match which {
            TradeRef::Ongoing(peer) => {
                let Some(t) = store.state.trades.get(&peer.to_hex()) else {
                    return;
                };
                let view = TradeView {
                    ongoing: true,
                    summary: t.offer_summary.clone().into(),
                    status: trade_status(t).into(),
                    started: format!(
                        "{} {}",
                        if t.started_by_me {
                            "you started it"
                        } else {
                            "they started it"
                        },
                        if t.started_at > 0 {
                            ago(Timestamp(t.started_at))
                        } else {
                            String::new()
                        }
                    )
                    .into(),
                    ..Default::default()
                };
                (peer, (view, t.messages.clone()))
            }
            TradeRef::Past(id) => {
                let Some(t) = store.history.get(id) else {
                    return;
                };
                let Ok(peer) = t.peer.parse::<PubKey>() else {
                    return;
                };
                let view = TradeView {
                    ongoing: false,
                    summary: t.offer_summary.clone().into(),
                    status: past_status(&t).into(),
                    started: format!(
                        "{} {}, ended {}",
                        if t.started_by_me {
                            "you started it"
                        } else {
                            "they started it"
                        },
                        ago(Timestamp(t.started_at)),
                        ago(Timestamp(t.ended_at))
                    )
                    .into(),
                    ..Default::default()
                };
                (peer, (view, t.messages))
            }
        };
        let (mut view, messages) = view;
        let s = &store.state;
        view.peer = peer.to_npub().into();
        view.peer_short = short_npub(&peer).into();
        view.label = s
            .labels
            .get(&peer.to_hex())
            .cloned()
            .unwrap_or_default()
            .into();
        view.blocked = s.blocked.contains(&peer.to_hex());
        drop(store);
        let msgs: Vec<MsgItem> = messages
            .iter()
            .map(|m| MsgItem {
                mine: m.mine,
                text: m.text.clone().into(),
                time: ago(Timestamp(m.created_at)).into(),
            })
            .collect();
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_trade(view);
            ui.set_messages(ModelRc::new(VecModel::from(msgs)));
        });
    }

    fn send_message(self: &Arc<Self>, text: &str) {
        if text.is_empty() || text.len() > 8000 {
            return;
        }
        let Some(TradeRef::Ongoing(peer)) = *self.current_trade.lock().unwrap() else {
            return;
        };
        if self.is_blocked(&peer) {
            self.status("unblock them first");
            return;
        }
        let conv = Conversation::new(&self.sk, peer);
        let (relays, needs_ping, evt) = {
            let mut store = self.store.lock().unwrap();
            let state = &mut store.state;
            let Some(t) = state.trades.get_mut(&peer.to_hex()) else {
                return;
            };
            let offer = t.offer_id.as_deref().and_then(|h| ID::from_hex(h).ok());
            let (rumor, evt) = conv.wrap(&self.me, text, offer);
            // until they answer we keep poking them, as they only start listening after a ping
            let needs_ping = !t.messages.iter().any(|m| !m.mine);
            let relays = t.relays.clone();
            t.messages.push(Message {
                id: rumor.id.clone(),
                created_at: rumor.created_at,
                mine: true,
                text: rumor.content,
            });
            state.my_rumors.insert(rumor.id);
            store.persist();
            (relays, needs_ping, evt)
        };
        self.render_trade();
        self.render_trades();

        if needs_ping {
            self.send_ping(peer);
        }
        let app = self.clone();
        self.rt.spawn(async move {
            if needs_ping {
                // give the other side a moment to open the conversation subscription
                tokio::time::sleep(Duration::from_millis(1500)).await;
            }
            let (ok, total) = app.publish(relays, evt).await;
            if ok.is_empty() {
                app.status(format!("message failed to reach any of {total} relays"));
            }
        });
    }

    /// end the ongoing trade: with a public report (success or failure), or just archived.
    /// either way it moves from the json state to the history database and we stop listening.
    fn end_trade(self: &Arc<Self>, outcome: Outcome, comment: &str) {
        let Some(TradeRef::Ongoing(peer)) = *self.current_trade.lock().unwrap() else {
            return;
        };
        {
            let mut store = self.store.lock().unwrap();
            let Some(trade) = store.state.trades.get(&peer.to_hex()).cloned() else {
                return;
            };
            if let Err(err) =
                store
                    .history
                    .add(&peer.to_hex(), &trade, outcome, comment, Timestamp::now().0)
            {
                drop(store);
                self.status(format!("failed to save the trade in the history: {err}"));
                return;
            }
            store.state.trades.remove(&peer.to_hex());
            store.persist();
        }
        self.dm_subs.lock().unwrap().remove(&peer.to_hex());
        if outcome != Outcome::Archived {
            self.publish_report(peer, outcome == Outcome::Success, comment);
        }
        self.leave_trade();
    }

    /// forget a trade without keeping it anywhere
    fn delete_trade(self: &Arc<Self>) {
        let Some(which) = *self.current_trade.lock().unwrap() else {
            return;
        };
        match which {
            TradeRef::Ongoing(peer) => {
                let mut store = self.store.lock().unwrap();
                store.state.trades.remove(&peer.to_hex());
                store.persist();
                drop(store);
                self.dm_subs.lock().unwrap().remove(&peer.to_hex());
            }
            TradeRef::Past(id) => self.store.lock().unwrap().history.delete(id),
        }
        self.leave_trade();
    }

    fn leave_trade(self: &Arc<Self>) {
        *self.current_trade.lock().unwrap() = None;
        self.render_trades();
        let _ = self.ui.upgrade_in_event_loop(|ui| ui.set_page(3));
    }

    /// the trades tab: our open offers, ongoing trades and the most recent past ones
    fn render_trades(&self) {
        let open: Vec<OfferItem> = self
            .open_offers()
            .iter()
            .map(|o| self.offer_item(o))
            .collect();
        let (ongoing, past) = {
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
                    let pk: PubKey = hex.parse().ok()?;
                    Some(TradeItem {
                        key: pk.to_npub().into(),
                        peer_short: short_npub(&pk).into(),
                        label: s.labels.get(hex).cloned().unwrap_or_default().into(),
                        summary: t.offer_summary.clone().into(),
                        status: trade_status(t).into(),
                        last: t
                            .messages
                            .last()
                            .map(|m| format!("{}{}", if m.mine { "you: " } else { "" }, m.text))
                            .unwrap_or_default()
                            .into(),
                        blocked: s.blocked.contains(hex),
                    })
                })
                .collect();
            let labels = s.labels.clone();
            let past: Vec<TradeItem> = store
                .history
                .list(PAST_TRADES_SHOWN)
                .iter()
                .filter_map(|t| {
                    let pk: PubKey = t.peer.parse().ok()?;
                    Some(TradeItem {
                        key: t.id.to_string().into(),
                        peer_short: short_npub(&pk).into(),
                        label: labels.get(&t.peer).cloned().unwrap_or_default().into(),
                        summary: t.offer_summary.clone().into(),
                        status: past_status(t).into(),
                        last: ago(Timestamp(t.ended_at)).into(),
                        blocked: false,
                    })
                })
                .collect();
            (ongoing, past)
        };
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_my_offers(ModelRc::new(VecModel::from(open)));
            ui.set_ongoing_trades(ModelRc::new(VecModel::from(ongoing)));
            ui.set_past_trades(ModelRc::new(VecModel::from(past)));
        });
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
            self.dm_subs.lock().unwrap().remove(&peer.to_hex());
            self.offers.lock().unwrap().retain(|o| o.maker != peer);
            self.render_offers();
        } else {
            self.ensure_dm_sub(peer, false);
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
        if peer == self.me {
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
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_profile_label(label.clone().into());
            // the offer page keeps a copy of the offer, so the back button would show the old label
            let mut offer = ui.get_current_offer();
            if offer.maker.as_str() == peer.to_npub() {
                offer.maker_label = label.into();
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
        let is_me = who == self.me;
        let label = self.label(&who);
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            if ui.get_page() != 5 {
                ui.set_back_page(ui.get_page());
            }
            ui.set_profile_npub(who.to_npub().into());
            ui.set_profile_short(short_npub(&who).into());
            ui.set_profile_blocked(blocked);
            ui.set_profile_label(label.into());
            ui.set_profile_is_me(is_me);
            ui.set_own_reports(ModelRc::default());
            ui.set_others_reports(ModelRc::default());
            ui.set_others_note("".into());
            ui.set_page(5);
        });
        self.render_profile(who, true);
    }

    /// our own reports about them first. if those are few, what our most frequent successful
    /// trade partners have said about them.
    fn render_profile(self: &Arc<Self>, who: PubKey, ask_relays: bool) {
        let is_me = who == self.me;
        let (own, trusted, others, labels) = {
            let store = self.store.lock().unwrap();
            let labels = store.state.labels.clone();
            if is_me {
                (store.reports_by(&self.me), vec![], vec![], labels)
            } else {
                let own = store.reports_about(&[self.me], &who);
                let trusted: Vec<PubKey> = if own.len() < ENOUGH_OWN_REPORTS {
                    store
                        .top_trade_partners(&self.me, TRUSTED_PEERS_TO_ASK + 1)
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
                    store.reports_about(&trusted, &who)
                };
                (own, trusted, others, labels)
            }
        };

        let note = if is_me {
            String::new()
        } else if own.len() >= ENOUGH_OWN_REPORTS {
            format!("you have {} reports of your own, that's enough.", own.len())
        } else if trusted.is_empty() {
            "you haven't reported any successful trades yet, so there is nobody you trust to ask."
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

        let own_items: Vec<ReportItem> =
            own.iter().map(|e| report_item(&labels, e, is_me)).collect();
        let other_items: Vec<ReportItem> = others
            .iter()
            .map(|e| report_item(&labels, e, false))
            .collect();

        let app = self.clone();
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            if *app.current_profile.lock().unwrap() != Some(who) {
                return;
            }
            ui.set_own_reports(ModelRc::new(VecModel::from(own_items)));
            ui.set_others_reports(ModelRc::new(VecModel::from(other_items)));
            ui.set_others_note(note.into());
        });

        if ask_relays && !trusted.is_empty() {
            let app = self.clone();
            self.rt.spawn(async move {
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

    fn report_trade(self: &Arc<Self>, success: bool, comment: &str) {
        let Some(peer) = *self.current_profile.lock().unwrap() else {
            return;
        };
        self.publish_report(peer, success, comment);
        self.render_profile(peer, false);
    }

    fn publish_report(self: &Arc<Self>, peer: PubKey, success: bool, comment: &str) {
        if peer == self.me {
            return;
        }
        let evt = trade_report(&self.sk, &peer, success, comment);
        self.store.lock().unwrap().save_event(&evt);

        let app = self.clone();
        self.rt.spawn(async move {
            let (ok, total) = app.publish(app.app_relays(), evt).await;
            app.status(format!(
                "trade report published to {}/{total} relays",
                ok.len()
            ));
        });
    }
}

// ---------------------------------------------------------------------------------- helpers

fn report_item(labels: &BTreeMap<String, String>, e: &Event, show_subject: bool) -> ReportItem {
    let comment = if show_subject {
        let subject = e
            .tags
            .find("p")
            .and_then(|t| t[1].parse::<PubKey>().ok())
            .map(|pk| display_name(labels, &pk))
            .unwrap_or_default();
        format!("with {subject}: {}", e.content)
    } else {
        e.content.clone()
    };
    ReportItem {
        author_short: display_name(labels, &e.pubkey).into(),
        success: e.kind == KIND_TRADE_SUCCESS,
        comment: comment.into(),
        time: ago(e.created_at).into(),
    }
}

fn trade_status(t: &Trade) -> String {
    let from_them = t.messages.iter().any(|m| !m.mine);
    let from_me = t.messages.iter().any(|m| m.mine);
    match t.messages.last() {
        _ if !from_them && (from_me || t.started_by_me) => "waiting for their answer".into(),
        _ if !from_me => "they are waiting for your answer".into(),
        Some(m) if !m.mine => "negotiating · your turn".into(),
        _ => "negotiating · their turn".into(),
    }
}

fn past_status(t: &PastTrade) -> String {
    let outcome = match t.outcome {
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

fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

fn display_name(labels: &BTreeMap<String, String>, pk: &PubKey) -> String {
    labels
        .get(&pk.to_hex())
        .cloned()
        .unwrap_or_else(|| short_npub(pk))
}

fn short_npub(pk: &PubKey) -> String {
    let npub = pk.to_npub();
    format!("{}…{}", &npub[..12], &npub[npub.len() - 6..])
}

fn ago(ts: Timestamp) -> String {
    let secs = Timestamp::now().0.saturating_sub(ts.0);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

fn thousands(v: f64) -> String {
    let int = v.round() as i64;
    let digits = int.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if int < 0 { format!("-{out}") } else { out }
}

#[cfg(test)]
mod tests {
    #[test]
    fn thousands() {
        assert_eq!(super::thousands(1234567.4), "1,234,567");
        assert_eq!(super::thousands(999.0), "999");
    }
}

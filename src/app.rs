//! glue between the ui, the relays and the local store

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ritualistic::{
    Event, Filter, ID, Network, PubKey, SecretKey, SubscriptionOptions, TagQuery, Timestamp,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::prices::{self, PriceApi};
use crate::protocol::*;
use crate::store::{ConversationState, ENOUGH_OWN_REPORTS, Message, Store, TRUSTED_PEERS_TO_ASK};
use crate::{AppWindow, ConvItem, MsgItem, OfferItem, PayItem, PersonItem, RelayItem, ReportItem};

const FETCH_TIMEOUT: Duration = Duration::from_secs(12);
const RECONNECT_DELAY: Duration = Duration::from_secs(10);

/// a live subscription, one task per relay so each one reconnects on its own. dropping it stops it.
struct LiveSub(Vec<tokio::task::AbortHandle>);

impl Drop for LiveSub {
    fn drop(&mut self) {
        for h in &self.0 {
            h.abort();
        }
    }
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
    /// (side, currency)
    filters: Mutex<(String, String)>,

    ping_sub: Mutex<Option<LiveSub>>,
    /// peer hex -> subscription to the conversation pubkey
    dm_subs: Mutex<HashMap<String, LiveSub>>,

    current_chat: Mutex<Option<PubKey>>,
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
        filters: Mutex::new(("all".into(), "all".into())),
        ping_sub: Mutex::new(None),
        dm_subs: Mutex::new(HashMap::new()),
        current_chat: Mutex::new(None),
        current_profile: Mutex::new(None),
    });

    setup_ui(&app, &ui);

    app.restart_ping_sub();
    let peers: Vec<String> = app
        .store
        .lock()
        .unwrap()
        .state
        .conversations
        .keys()
        .cloned()
        .collect();
    for peer in peers {
        if let Ok(pk) = peer.parse() {
            app.ensure_dm_sub(pk, false);
        }
    }
    app.refresh_offers();

    ui.run()
}

fn setup_ui(app: &Arc<App>, ui: &AppWindow) {
    ui.set_npub(app.me.to_npub().into());

    let mut currencies: Vec<SharedString> = vec!["all".into()];
    currencies.extend(prices::currency_codes().into_iter().map(SharedString::from));
    ui.set_currencies(ModelRc::new(VecModel::from(currencies)));
    // the combobox drops a value that isn't in its model yet, so set it again now that it is
    ui.set_filter_currency("all".into());

    ui.set_payment_options(checklist(PAYMENT_METHODS));
    ui.set_bitcoin_options(checklist(BITCOIN_METHODS));
    set_apis_for(ui, "USD");
    app.render_relays();
    app.render_conversations();
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

    let weak = ui.as_weak();
    ui.on_create_currency_changed(move |cur| {
        if let Some(ui) = weak.upgrade() {
            set_apis_for(&ui, &cur);
        }
    });

    let weak = ui.as_weak();
    ui.on_toggle_payment(move |idx, checked| {
        if let Some(ui) = weak.upgrade() {
            set_checked(&ui.get_payment_options(), idx, checked);
        }
    });

    let weak = ui.as_weak();
    ui.on_toggle_bitcoin(move |idx, checked| {
        if let Some(ui) = weak.upgrade() {
            set_checked(&ui.get_bitcoin_options(), idx, checked);
        }
    });

    let a = app.clone();
    ui.on_preview_price(move || a.preview_price());

    let a = app.clone();
    ui.on_publish_offer(move || a.publish_offer());

    let a = app.clone();
    ui.on_open_chat(move |peer| {
        if let Ok(pk) = peer.parse::<PubKey>() {
            a.open_chat(pk);
        }
    });

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

fn checklist(names: &[&str]) -> ModelRc<PayItem> {
    ModelRc::new(VecModel::from(
        names
            .iter()
            .map(|name| PayItem {
                name: (*name).into(),
                checked: false,
            })
            .collect::<Vec<_>>(),
    ))
}

fn set_checked(model: &ModelRc<PayItem>, idx: i32, checked: bool) {
    if let Some(mut item) = model.row_data(idx as usize) {
        item.checked = checked;
        model.set_row_data(idx as usize, item);
    }
}

fn checked_names(model: &ModelRc<PayItem>) -> Vec<String> {
    model
        .iter()
        .filter(|p| p.checked)
        .map(|p| p.name.to_string())
        .collect()
}

fn set_apis_for(ui: &AppWindow, currency: &str) {
    let apis: Vec<SharedString> = prices::apis_for(currency)
        .iter()
        .map(|a| a.id().into())
        .collect();
    if let Some(first) = apis.first() {
        ui.set_create_api(first.clone());
    }
    ui.set_create_apis(ModelRc::new(VecModel::from(apis)));
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
        store.state.labels.get(&pk.to_hex()).cloned().unwrap_or_default()
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

    async fn publish(&self, relays: Vec<String>, evt: Event) -> (usize, usize) {
        let total = relays.len();
        let mut results = self.net.clone().publish_many(&relays, evt).await;
        let mut ok = 0;
        while let Some(res) = results.recv().await {
            match res.error {
                None => ok += 1,
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
        let (known, enabled) = {
            let s = &self.store.lock().unwrap().state;
            let mut known = s.known_relays.clone();
            for r in &s.app_relays {
                if !known.contains(r) {
                    known.push(r.clone());
                }
            }
            (known, s.app_relays.clone())
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
            ui.set_create_relays(enabled.join(", ").into());
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
            offers.sort_by_key(|o| std::cmp::Reverse(o.created_at));
            let count = offers.len();
            *app.offers.lock().unwrap() = offers;
            let _ = app.ui.upgrade_in_event_loop(|ui| ui.set_loading(false));
            app.status(format!("{count} offers from {} relays", relays.len()));
            app.render_offers();
            app.fetch_prices().await;
        });
    }

    async fn fetch_prices(self: &Arc<Self>) {
        let pairs: HashSet<(String, String)> = self
            .offers
            .lock()
            .unwrap()
            .iter()
            .map(|o| (o.currency.clone(), o.price_api.clone()))
            .collect();
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
                    self.prices.lock().unwrap().insert(key, price);
                }
                Ok(((cur, api), Err(err))) => log::warn!("price {cur}/{api}: {err}"),
                Err(_) => {}
            }
        }
        self.render_offers();
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

    fn preview_price(self: &Arc<Self>) {
        let Some(ui) = self.ui.upgrade() else { return };
        let cur = ui.get_create_currency().to_string();
        let Some(premium) = prices::parse_premium(&ui.get_create_premium()) else {
            ui.set_create_preview("invalid premium, use something like +1% or -0.5%".into());
            return;
        };
        let Some(api) = PriceApi::from_id(&ui.get_create_api()) else {
            return;
        };
        ui.set_create_preview("fetching market price…".into());
        let weak = self.ui.clone();
        self.rt.spawn(async move {
            let text = match api.fetch(&cur).await {
                Ok(m) => format!(
                    "market: {} {cur}/BTC → your price: {} {cur}/BTC",
                    thousands(m),
                    thousands(prices::apply_premium(m, premium))
                ),
                Err(err) => format!("failed to fetch price: {err}"),
            };
            let _ = weak.upgrade_in_event_loop(move |ui| ui.set_create_preview(text.into()));
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
            let ping_relays = &mut store.state.ping_relays;
            for r in relays.into_iter().rev() {
                ping_relays.retain(|x| x != &r);
                ping_relays.insert(0, r);
            }
            ping_relays.truncate(12);
            store.persist();
        }
        self.restart_ping_sub();

        ui.set_create_description("".into());
        ui.set_page(0);
        self.status("publishing offer…");

        let app = self.clone();
        self.rt.spawn(async move {
            let (ok, total) = app.publish(app.app_relays(), evt).await;
            app.status(format!("offer published to {ok}/{total} relays"));
        });
    }

    // ------------------------------------------------------------------------------ dms

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

    fn on_ping(self: &Arc<Self>, evt: Event) {
        let peer = evt.pubkey;
        if peer == self.me || !evt.verify_signature() || self.is_blocked(&peer) {
            return;
        }
        let is_new = {
            let mut store = self.store.lock().unwrap();
            let is_new = !store.state.conversations.contains_key(&peer.to_hex());
            if is_new {
                let relays = if store.state.ping_relays.is_empty() {
                    store.state.app_relays.clone()
                } else {
                    store.state.ping_relays.clone()
                };
                store.state.conversations.insert(
                    peer.to_hex(),
                    ConversationState {
                        relays,
                        offer_summary: "pinged you about your offer".into(),
                        ..Default::default()
                    },
                );
                store.persist();
            }
            is_new
        };
        self.ensure_dm_sub(peer, false);
        if is_new {
            self.status(format!("new chat from {}", self.name(&peer)));
            self.render_conversations();
        }
    }

    /// subscribe to `authors:[conversation pubkey]`, which is all it takes to hear from a peer.
    /// blocking someone is just never doing this for them.
    fn ensure_dm_sub(self: &Arc<Self>, peer: PubKey, restart: bool) {
        if self.is_blocked(&peer) {
            return;
        }
        let hex = peer.to_hex();
        let mut subs = self.dm_subs.lock().unwrap();
        if subs.contains_key(&hex) && !restart {
            return;
        }
        let Some(relays) = self.conversation_relays(&peer) else {
            return;
        };
        let conv = Conversation::new(&self.sk, peer);
        let filter = Filter {
            kinds: Some(vec![KIND_DM]),
            authors: Some(vec![conv.pubkey()]),
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
            let Some(c) = store.state.conversations.get_mut(&conv.peer.to_hex()) else {
                return;
            };
            if c.messages.iter().any(|m| m.id == rumor.id) {
                return;
            }
            c.messages.push(Message {
                id: rumor.id,
                created_at: rumor.created_at,
                mine: false,
                text: rumor.content,
            });
            c.messages.sort_by_key(|m| m.created_at);
            store.persist();
        }
        self.render_conversations();
        if *self.current_chat.lock().unwrap() == Some(conv.peer) {
            self.render_chat();
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
            let c = store
                .state
                .conversations
                .entry(offer.maker.to_hex())
                .or_default();
            c.relays = offer.relays.clone();
            c.offer_id = Some(offer.id.to_hex());
            c.offer_summary = format!(
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
        self.render_conversations();
        self.open_chat(offer.maker);
    }

    fn send_ping(self: &Arc<Self>, peer: PubKey) {
        let Some(relays) = self.conversation_relays(&peer) else {
            return;
        };
        let evt = ping(&self.sk, &peer);
        let app = self.clone();
        self.rt.spawn(async move {
            app.publish(relays, evt).await;
        });
    }

    fn conversation_relays(&self, peer: &PubKey) -> Option<Vec<String>> {
        self.store
            .lock()
            .unwrap()
            .state
            .conversations
            .get(&peer.to_hex())
            .map(|c| c.relays.clone())
    }

    fn open_chat(self: &Arc<Self>, peer: PubKey) {
        if peer == self.me {
            return;
        }
        let created = {
            let mut store = self.store.lock().unwrap();
            let exists = store.state.conversations.contains_key(&peer.to_hex());
            if !exists {
                // talking to someone outside of an offer, use our own relays
                let relays = store.state.app_relays.clone();
                store.state.conversations.insert(
                    peer.to_hex(),
                    ConversationState {
                        relays,
                        offer_summary: "direct chat".into(),
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
            self.render_conversations();
        }
        *self.current_chat.lock().unwrap() = Some(peer);
        self.render_chat();
        let _ = self.ui.upgrade_in_event_loop(|ui| {
            ui.set_back_page(3);
            ui.set_page(4);
        });
    }

    fn render_chat(&self) {
        let Some(peer) = *self.current_chat.lock().unwrap() else {
            return;
        };
        let (summary, msgs) = {
            let store = self.store.lock().unwrap();
            let Some(c) = store.state.conversations.get(&peer.to_hex()) else {
                return;
            };
            let msgs: Vec<MsgItem> = c
                .messages
                .iter()
                .map(|m| MsgItem {
                    mine: m.mine,
                    text: m.text.clone().into(),
                    time: ago(Timestamp(m.created_at)).into(),
                })
                .collect();
            (c.offer_summary.clone(), msgs)
        };
        let label = self.label(&peer);
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_chat_peer(peer.to_npub().into());
            ui.set_chat_peer_short(short_npub(&peer).into());
            ui.set_chat_peer_label(label.into());
            ui.set_chat_summary(summary.into());
            ui.set_messages(ModelRc::new(VecModel::from(msgs)));
        });
    }

    fn send_message(self: &Arc<Self>, text: &str) {
        if text.is_empty() || text.len() > 8000 {
            return;
        }
        let Some(peer) = *self.current_chat.lock().unwrap() else {
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
            let Some(c) = state.conversations.get_mut(&peer.to_hex()) else {
                return;
            };
            let offer = c.offer_id.as_deref().and_then(|h| ID::from_hex(h).ok());
            let (rumor, evt) = conv.wrap(&self.me, text, offer);
            // until they answer we keep poking them, as they only start listening after a ping
            let needs_ping = !c.messages.iter().any(|m| !m.mine);
            let relays = c.relays.clone();
            c.messages.push(Message {
                id: rumor.id.clone(),
                created_at: rumor.created_at,
                mine: true,
                text: rumor.content,
            });
            state.my_rumors.insert(rumor.id);
            store.persist();
            (relays, needs_ping, evt)
        };
        self.render_chat();
        self.render_conversations();

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
            if ok == 0 {
                app.status(format!("message failed to reach any of {total} relays"));
            }
        });
    }

    fn render_conversations(&self) {
        let items: Vec<ConvItem> = {
            let s = &self.store.lock().unwrap().state;
            let mut convs: Vec<_> = s.conversations.iter().collect();
            convs.sort_by_key(|(_, c)| std::cmp::Reverse(c.messages.last().map(|m| m.created_at)));
            convs
                .into_iter()
                .filter_map(|(hex, c)| {
                    let pk: PubKey = hex.parse().ok()?;
                    Some(ConvItem {
                        peer: pk.to_npub().into(),
                        peer_short: short_npub(&pk).into(),
                        label: s.labels.get(hex).cloned().unwrap_or_default().into(),
                        summary: c.offer_summary.clone().into(),
                        last: c
                            .messages
                            .last()
                            .map(|m| format!("{}{}", if m.mine { "you: " } else { "" }, m.text))
                            .unwrap_or_default()
                            .into(),
                        blocked: s.blocked.contains(hex),
                    })
                })
                .collect()
        };
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_conversations(ModelRc::new(VecModel::from(items)));
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
        self.render_conversations();
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
        self.render_conversations();
        self.render_chat();
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

        let own_items: Vec<ReportItem> = own
            .iter()
            .map(|e| report_item(&labels, e, is_me))
            .collect();
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
        if peer == self.me {
            return;
        }
        let evt = trade_report(&self.sk, &peer, success, comment);
        self.store.lock().unwrap().save_event(&evt);
        self.render_profile(peer, false);

        let app = self.clone();
        self.rt.spawn(async move {
            let (ok, total) = app.publish(app.app_relays(), evt).await;
            app.status(format!("trade report published to {ok}/{total} relays"));
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

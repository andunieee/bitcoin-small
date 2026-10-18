//! XFK, a pretend currency to practice with. its market is a relay inside the app
//! (`relays::MemoryRelay`), where three bots trade with whoever shows up: a seller and a buyer that
//! keep an offer up, and a taker that takes the XFK offers we make. they speak the same protocol as
//! everyone else, through the same `Relays` interface, and nothing they say leaves the device.
//!
//! each bot has a key of its own, made on first launch and kept next to ours. what they need to
//! pick up their trades after a restart is in a file of their own too, and the rest they read back
//! from the relay, which keeps its events.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ritualistic::{Event, Filter, ID, PubKey, SecretKey, TagQuery, Timestamp};
use tokio_with_wasm::alias as tokio;

use crate::deal::{self, Action, Deal, OfferTerms, Step};
use crate::protocol::*;
use crate::relays::{Occurrence, Relays};
use crate::store::{Files, Message, load_or_create_key};

pub const XFK: &str = "XFK";
/// where the practice market is. `.invalid` never resolves, so it can't reach a real relay.
pub const RELAY: &str = "wss://xfk.invalid/";
pub const PRICE_API: &str = "xfk";
/// what a bitcoin costs in XFK, always: 1 XFK is 1,000 sats
pub const PRICE: f64 = 100_000.0;

/// the bots' offers are for anything between these
const AMOUNT_MIN: f64 = 1.0;
const AMOUNT_MAX: f64 = 50.0;
/// how often the seller and the buyer check that their offer is up, and how much life it needs left
const OFFER_CHECK: Duration = Duration::from_secs(10 * 60);
const OFFER_MIN_LEFT_SECS: u32 = 60 * 60;
/// how often the taker looks for offers to take, which is also about how long it takes to notice one
const TAKE_CHECK: Duration = Duration::from_secs(3);
/// a trade nobody said anything in for this long is over for the taker, who moves on
const ABANDONED_SECS: u32 = 30 * 60;
/// the user pings again with each message until we answer, those are about the same trade
const REPING_SECS: u32 = 60;

pub fn is_xfk(currency: &str) -> bool {
    currency.eq_ignore_ascii_case(XFK)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Persona {
    Seller,
    Buyer,
    Taker,
}

const PERSONAS: [Persona; 3] = [Persona::Seller, Persona::Buyer, Persona::Taker];

impl Persona {
    fn name(self) -> &'static str {
        match self {
            Persona::Seller => "seller",
            Persona::Buyer => "buyer",
            Persona::Taker => "taker",
        }
    }

    fn key_file(self) -> String {
        format!("xfk-{}.key", self.name())
    }

    fn state_file(self) -> String {
        format!("xfk-{}.json", self.name())
    }

    /// the offer the seller and the buyer keep up
    fn offer(self) -> Option<NewOffer> {
        let side = match self {
            Persona::Seller => Side::Sell,
            Persona::Buyer => Side::Buy,
            Persona::Taker => return None,
        };
        Some(NewOffer {
            side,
            currency: XFK.into(),
            price_api: PRICE_API.into(),
            premium: 0.0,
            amount_min: AMOUNT_MIN,
            amount_max: AMOUNT_MAX,
            payment_methods: vec!["Play money".into(), "Bank transfer".into()],
            bitcoin_methods: BITCOIN_METHODS.iter().map(|m| m.to_string()).collect(),
            relays: vec![RELAY.into()],
            description: format!(
                "a pretend {} to practice with: XFK isn't real money and nothing is sent for real. \
                 take this offer and go through a whole trade.",
                self.name()
            ),
        })
    }

    fn hello(self) -> &'static str {
        match self {
            Persona::Seller | Persona::Buyer => {
                "hello! I'm a pretend trader, here so you can practice. XFK isn't real money and \
                 no bitcoin moves: pick an amount and the methods, give your details, say when \
                 you've sent and I'll do my part."
            }
            Persona::Taker => {
                "hello! I'm a pretend taker, here so you can practice the maker's side. XFK isn't \
                 real money and no bitcoin moves: I'll pick what I want from your offer, then give \
                 your details and send your part when it's your turn."
            }
        }
    }
}

const BYE: &str = "that was the whole trade! nothing was sent for real. now conclude it on your \
                   side: with real people, that is where you say how it went.";

/// where the bots want their pretend fiat
const PAYMENT_DETAILS: &str = "pretend account 0000 1234 5678, Xfk Bot. nothing to pay for real.";

/// where the bots want their pretend bitcoin, for each method that checks the format
fn bitcoin_destination(method: &str) -> &'static str {
    match method {
        "on-chain" | "onchain" => "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
        "lightning" => "xfkbot@example.com",
        "liquid" => "ex1q7gkeyjut0mrxc3j0kjlt7rmcnvsh0gt45d3fud",
        "arkade" => {
            "ark1qv9pzxqlyckngw6zf9g9whn9d3eh4qvg37tfmf9tk2uup37w6hww86h3lrlsvrg5rv3zjvph8ez5c566v95x7anasj9e9xdq57htt0xrf8m2qn"
        }
        "rootstock" => "0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed",
        _ => "anywhere is fine, this is practice",
    }
}

/// the bots' pubkeys, creating their keys if they don't exist yet
pub fn people(files: &dyn Files) -> Vec<PubKey> {
    PERSONAS
        .iter()
        .map(|p| load_or_create_key(files, &p.key_file()).pubkey())
        .collect()
}

/// start the bots, talking to `relay` and nothing else
pub fn start(relay: Arc<dyn Relays>, files: Arc<dyn Files>) {
    let bots = people(&*files);
    for persona in PERSONAS {
        let bot = Bot::load(persona, relay.clone(), files.clone(), bots.clone());
        tokio::spawn(async move { bot.run().await });
    }
}

/// what a bot keeps between runs
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Saved {
    /// peer hex -> the trade with them
    trades: HashMap<String, SavedTrade>,
    /// the offers of others the taker took, so it takes each one once
    taken: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SavedTrade {
    terms: OfferTerms,
    /// the offer, for the taker to point at it in its messages
    offer_id: Option<String>,
    started_at: u32,
}

/// a trade going on, rebuilt from the relay
struct Talk {
    saved: SavedTrade,
    conv: Conversation,
    messages: Vec<Message>,
    /// while a task is acting on it
    busy: bool,
    sub: tokio::task::AbortHandle,
}

impl Drop for Talk {
    fn drop(&mut self) {
        self.sub.abort();
    }
}

impl Talk {
    fn deal(&self) -> Deal {
        Deal::replay(&self.saved.terms, &self.messages)
    }

    fn last_activity(&self) -> u32 {
        self.messages
            .last()
            .map_or(self.saved.started_at, |m| m.created_at)
    }

    /// what to say next, how long to take, and the action that goes with it, if any
    fn next_move(&self) -> Option<(String, Option<Action>, Duration)> {
        let terms = &self.saved.terms;
        let me = terms.my_role();
        let secs = Duration::from_secs;
        if !self.messages.iter().any(|m| m.mine) {
            return Some((String::new(), None, secs(2)));
        }
        let deal = self.deal();
        if deal.concluded() {
            let said_bye = self.messages.iter().any(|m| m.mine && m.text == BYE);
            return (!said_bye).then(|| (BYE.to_string(), None, secs(3)));
        }
        let step = deal.available(terms, me).into_iter().next()?;
        let (action, delay) = match step {
            Step::Amount => (Action::Amount(self.pick_amount()), secs(2)),
            Step::Price => (Action::Price(PRICE), secs(2)),
            Step::BitcoinMethod => {
                let pick = terms
                    .bitcoin_methods
                    .iter()
                    .find(|m| *m == "lightning")
                    .or(terms.bitcoin_methods.first())?;
                (Action::BitcoinMethod(pick.clone()), secs(2))
            }
            Step::PaymentMethod => (
                Action::PaymentMethod(terms.payment_methods.first()?.clone()),
                secs(2),
            ),
            Step::BitcoinDetails => {
                let method = deal.bitcoin_method.as_deref().unwrap_or("");
                let details = bitcoin_destination(method).to_string();
                (Action::BitcoinDetails(details), secs(3))
            }
            Step::PaymentDetails => (Action::PaymentDetails(PAYMENT_DETAILS.into()), secs(3)),
            // as if it took a while to send it
            Step::TakerSends | Step::MakerSends => (Action::Sent, secs(8)),
        };
        // never something the other side would ignore
        deal.check(terms, me, &action).ok()?;
        Some((action.text(terms, me, &deal), Some(action), delay))
    }

    /// a round amount in the offer's range, the same for as long as the trade lasts
    fn pick_amount(&self) -> f64 {
        let terms = &self.saved.terms;
        let (min, max) = (terms.amount_min.ceil(), terms.amount_max.floor());
        if terms.fixed_amount() || min > max {
            return terms.amount_min;
        }
        let options = (max - min) as u32 + 1;
        min + (self.saved.started_at % options) as f64
    }
}

struct Bot {
    persona: Persona,
    sk: SecretKey,
    me: PubKey,
    relay: Arc<dyn Relays>,
    files: Arc<dyn Files>,
    /// all the bots, who don't trade with each other
    bots: Vec<PubKey>,
    talks: Mutex<HashMap<PubKey, Talk>>,
    taken: Mutex<Vec<String>>,
}

impl Bot {
    fn load(
        persona: Persona,
        relay: Arc<dyn Relays>,
        files: Arc<dyn Files>,
        bots: Vec<PubKey>,
    ) -> Arc<Self> {
        let sk = load_or_create_key(&*files, &persona.key_file());
        let saved: Saved = files
            .read(&persona.state_file())
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        let bot = Arc::new(Bot {
            persona,
            me: sk.pubkey(),
            sk,
            relay,
            files,
            bots,
            talks: Mutex::new(HashMap::new()),
            taken: Mutex::new(saved.taken),
        });
        for (hex, trade) in saved.trades {
            if let Ok(peer) = hex.parse() {
                bot.begin(peer, trade);
            }
        }
        bot
    }

    fn relays(&self) -> Vec<String> {
        vec![RELAY.to_string()]
    }

    fn persist(&self) {
        let saved = Saved {
            trades: self
                .talks
                .lock()
                .unwrap()
                .iter()
                .map(|(pk, t)| (pk.to_hex(), t.saved.clone()))
                .collect(),
            taken: self.taken.lock().unwrap().clone(),
        };
        let res = serde_json::to_string(&saved)
            .map_err(std::io::Error::other)
            .and_then(|json| self.files.write(&self.persona.state_file(), &json));
        if let Err(err) = res {
            log::warn!("the XFK {} failed to save its state: {err}", self.persona.name());
        }
    }

    async fn run(self: Arc<Self>) {
        if self.persona == Persona::Taker {
            loop {
                tokio::time::sleep(TAKE_CHECK).await;
                self.take_offers().await;
            }
        }
        let bot = self.clone();
        tokio::spawn(async move { bot.listen_for_pings().await });
        loop {
            self.keep_offer_up().await;
            tokio::time::sleep(OFFER_CHECK).await;
        }
    }

    /// the stored events that match
    async fn query(&self, filter: Filter) -> Vec<Event> {
        let mut sub = self.relay.subscribe(&self.relays(), filter);
        let mut events = vec![];
        while let Some(Occurrence::Event(evt)) = sub.recv().await {
            events.push(evt);
        }
        events
    }

    fn publish(&self, evt: Event) {
        // nobody to tell if it failed
        drop(self.relay.publish(&self.relays(), evt));
    }

    /// publish a new offer when ours is about to expire, and take the old one down
    async fn keep_offer_up(&self) {
        let Some(offer) = self.persona.offer() else {
            return;
        };
        let up: Vec<Offer> = self
            .query(Filter {
                kinds: Some(vec![KIND_OFFER]),
                authors: Some(vec![self.me]),
                ..Default::default()
            })
            .await
            .iter()
            .filter_map(Offer::from_event)
            .collect();
        if up
            .iter()
            .any(|o| o.expiration.0 > now().0 + OFFER_MIN_LEFT_SECS)
        {
            return;
        }
        self.publish(offer.to_event(&self.sk));
        for old in up {
            self.publish(deletion(&self.sk, &old.id, KIND_OFFER));
        }
    }

    async fn listen_for_pings(self: Arc<Self>) {
        let mut sub = self.relay.subscribe(
            &self.relays(),
            Filter {
                kinds: Some(vec![KIND_PING]),
                tags: Some(vec![TagQuery("p".into(), vec![self.me.to_hex()])]),
                since: Some(now()),
                ..Default::default()
            },
        );
        while let Some(occ) = sub.recv().await {
            if let Occurrence::Event(evt) = occ {
                self.on_ping(evt);
            }
        }
    }

    /// someone took our offer: a new trade, unless it is the one going on
    fn on_ping(self: &Arc<Self>, evt: Event) {
        let peer = evt.pubkey;
        if peer == self.me || self.bots.contains(&peer) || !evt.verify_signature() {
            return;
        }
        let Some(offer) = self.persona.offer() else {
            return;
        };
        let same_trade = self.talks.lock().unwrap().get(&peer).is_some_and(|t| {
            !t.deal().concluded() && evt.created_at.0 < t.saved.started_at + REPING_SECS
        });
        if same_trade {
            return;
        }
        let terms = OfferTerms {
            mine: true,
            side: offer.side,
            currency: offer.currency,
            price_api: offer.price_api,
            premium: offer.premium,
            amount_min: offer.amount_min,
            amount_max: offer.amount_max,
            payment_methods: offer.payment_methods,
            bitcoin_methods: offer.bitcoin_methods,
        };
        self.begin(
            peer,
            SavedTrade {
                terms,
                offer_id: None,
                started_at: evt.created_at.0,
            },
        );
        self.persist();
        self.act(peer);
    }

    /// take the XFK offers of whoever isn't a bot, one at a time with each of them
    async fn take_offers(self: &Arc<Self>) {
        let since = Timestamp(now().0.saturating_sub(OFFER_TTL_SECS));
        let mut offers: Vec<Offer> = self
            .query(Filter {
                kinds: Some(vec![KIND_OFFER]),
                since: Some(since),
                ..Default::default()
            })
            .await
            .iter()
            .filter(|e| e.verify_signature())
            .filter_map(Offer::from_event)
            .filter(|o| is_xfk(&o.currency) && !self.bots.contains(&o.maker))
            .collect();
        offers.sort_by_key(|o| o.created_at);
        for offer in offers {
            if self.taken.lock().unwrap().contains(&offer.id.to_hex()) {
                continue;
            }
            let busy = self.talks.lock().unwrap().get(&offer.maker).is_some_and(|t| {
                !t.deal().concluded() && t.last_activity() + ABANDONED_SECS > now().0
            });
            if busy {
                continue;
            }
            {
                let mut taken = self.taken.lock().unwrap();
                taken.push(offer.id.to_hex());
                let excess = taken.len().saturating_sub(100);
                taken.drain(..excess);
            }
            let started_at = now().0;
            self.begin(
                offer.maker,
                SavedTrade {
                    terms: OfferTerms::from_offer(&offer, false),
                    offer_id: Some(offer.id.to_hex()),
                    started_at,
                },
            );
            self.persist();
            // the maker only listens to us after this
            self.publish(ping(&self.sk, &offer.maker));
            self.act(offer.maker);
        }
    }

    /// listen to the conversation with `peer`, replacing whatever trade there was with them
    fn begin(self: &Arc<Self>, peer: PubKey, saved: SavedTrade) {
        let conv = Conversation::new(&self.sk, peer);
        let filter = Filter {
            kinds: Some(vec![KIND_DM]),
            authors: Some(vec![conv.pubkey()]),
            // everything happens on one clock here, so no slack: older ones are from a past trade
            since: Some(Timestamp(saved.started_at)),
            ..Default::default()
        };
        let bot = self.clone();
        let sub = tokio::spawn(async move {
            let mut sub = bot.relay.subscribe(&bot.relays(), filter);
            while let Some(occ) = sub.recv().await {
                if let Occurrence::Event(evt) = occ {
                    bot.on_dm(peer, evt);
                }
            }
        })
        .abort_handle();
        self.talks.lock().unwrap().insert(
            peer,
            Talk {
                saved,
                conv,
                messages: vec![],
                busy: false,
                sub,
            },
        );
    }

    fn on_dm(self: &Arc<Self>, peer: PubKey, evt: Event) {
        {
            let mut talks = self.talks.lock().unwrap();
            let Some(t) = talks.get_mut(&peer) else {
                return;
            };
            let Some(rumor) = t.conv.unwrap(&evt) else {
                return;
            };
            if rumor.created_at < t.saved.started_at || t.messages.iter().any(|m| m.id == rumor.id)
            {
                return;
            }
            // the app can't tell who wrote what, but nobody forges anything in here, so the rumor's
            // author is good enough for us
            let mine = rumor.pubkey == self.me.to_hex();
            let action = rumor
                .tags
                .iter()
                .find(|t| t.first().is_some_and(|n| n == deal::TAG))
                .cloned();
            t.messages.push(Message {
                id: rumor.id,
                created_at: rumor.created_at,
                mine,
                text: rumor.content,
                action,
                ..Default::default()
            });
            t.messages.sort_by_key(|m| m.created_at);
        }
        self.act(peer);
    }

    /// take our next steps in the trade with `peer`, one by one, unless we are at it already
    fn act(self: &Arc<Self>, peer: PubKey) {
        let started_at = {
            let mut talks = self.talks.lock().unwrap();
            let Some(t) = talks.get_mut(&peer) else {
                return;
            };
            if t.busy {
                return;
            }
            t.busy = true;
            t.saved.started_at
        };
        let bot = self.clone();
        tokio::spawn(async move {
            loop {
                let planned = {
                    let mut talks = bot.talks.lock().unwrap();
                    let Some(t) = talks.get_mut(&peer) else {
                        return;
                    };
                    match t.next_move() {
                        Some(m) => m,
                        None => {
                            t.busy = false;
                            return;
                        }
                    }
                };
                tokio::time::sleep(planned.2).await;
                // a new trade with them, or something arrived that changed our mind
                let evt = {
                    let mut talks = bot.talks.lock().unwrap();
                    let Some(t) = talks.get_mut(&peer) else {
                        return;
                    };
                    if t.saved.started_at != started_at {
                        return;
                    }
                    let Some((text, action, _)) = t.next_move() else {
                        continue;
                    };
                    if (&text, &action) != (&planned.0, &planned.1) {
                        continue;
                    }
                    bot.say(t, text, action)
                };
                bot.publish(evt);
            }
        });
    }

    /// a message in `t`, kept as ours right away so the next move knows about it
    fn say(&self, t: &mut Talk, text: String, action: Option<Action>) -> Event {
        let text = if text.is_empty() {
            self.persona.hello().to_string()
        } else {
            text
        };
        let offer = t.saved.offer_id.as_deref().and_then(|h| ID::from_hex(h).ok());
        let action = action.map(|a| a.to_tag());
        let (rumor, evt) = t.conv.wrap(&self.me, &text, offer, action.clone());
        t.messages.push(Message {
            id: rumor.id,
            created_at: rumor.created_at,
            mine: true,
            text: rumor.content,
            action,
            ..Default::default()
        });
        evt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relays::MemoryRelay;

    struct MemFiles(Mutex<HashMap<String, String>>);

    impl Files for MemFiles {
        fn read(&self, name: &str) -> Option<String> {
            self.0.lock().unwrap().get(name).cloned()
        }

        fn write(&self, name: &str, contents: &str) -> std::io::Result<()> {
            self.0.lock().unwrap().insert(name.into(), contents.into());
            Ok(())
        }
    }

    #[test]
    fn the_bots_have_valid_destinations() {
        for m in BITCOIN_METHODS {
            assert!(
                crate::address::parse(m, bitcoin_destination(m), Some(1000)).is_ok(),
                "{m}"
            );
        }
    }

    #[test]
    fn keys_are_kept() {
        let files = MemFiles(Mutex::new(HashMap::new()));
        let first = people(&files);
        assert_eq!(first.len(), 3);
        assert_ne!(first[0], first[1]);
        assert_eq!(people(&files), first);
    }

    /// us, as the app would be: a key, a conversation, and the messages of a trade
    struct Me {
        sk: SecretKey,
        relay: Arc<MemoryRelay>,
        terms: OfferTerms,
        conv: Conversation,
        offer: Option<ID>,
        messages: Vec<Message>,
        sub: tokio::sync::mpsc::UnboundedReceiver<Occurrence>,
    }

    impl Me {
        fn new(sk: SecretKey, relay: Arc<MemoryRelay>, peer: PubKey, terms: OfferTerms) -> Self {
            let conv = Conversation::new(&sk, peer);
            let sub = relay.subscribe(
                &[],
                Filter {
                    kinds: Some(vec![KIND_DM]),
                    authors: Some(vec![conv.pubkey()]),
                    ..Default::default()
                },
            );
            Me {
                sk,
                relay,
                terms,
                conv,
                offer: None,
                messages: vec![],
                sub,
            }
        }

        fn deal(&self) -> Deal {
            Deal::replay(&self.terms, &self.messages)
        }

        /// wait for the next message from them
        async fn hear(&mut self) -> Message {
            loop {
                let occ = tokio::time::timeout(Duration::from_secs(20), self.sub.recv())
                    .await
                    .expect("they took too long")
                    .expect("the subscription ended");
                let Occurrence::Event(evt) = occ else { continue };
                let rumor = self.conv.unwrap(&evt).unwrap();
                if rumor.pubkey == self.sk.pubkey().to_hex() {
                    continue;
                }
                let m = Message {
                    id: rumor.id,
                    created_at: rumor.created_at,
                    mine: false,
                    text: rumor.content,
                    action: rumor.tags.iter().find(|t| t[0] == deal::TAG).cloned(),
                    ..Default::default()
                };
                self.messages.push(m.clone());
                return m;
            }
        }

        fn act(&mut self, action: Action) {
            let me = self.terms.my_role();
            let deal = self.deal();
            deal.check(&self.terms, me, &action).unwrap();
            let text = action.text(&self.terms, me, &deal);
            let tag = action.to_tag();
            let (rumor, evt) = self
                .conv
                .wrap(&self.sk.pubkey(), &text, self.offer, Some(tag.clone()));
            self.messages.push(Message {
                id: rumor.id,
                created_at: rumor.created_at,
                mine: true,
                text,
                action: Some(tag),
                ..Default::default()
            });
            self.relay.publish(&[], evt);
        }
    }

    fn bot_offer(relay: &MemoryRelay, maker: &PubKey) -> Offer {
        let mut sub = relay.subscribe(
            &[],
            Filter {
                kinds: Some(vec![KIND_OFFER]),
                authors: Some(vec![*maker]),
                ..Default::default()
            },
        );
        match sub.try_recv() {
            Ok(Occurrence::Event(evt)) => Offer::from_event(&evt).unwrap(),
            other => panic!("no offer: {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn buying_from_the_seller() {
        let files: Arc<dyn Files> = Arc::new(MemFiles(Mutex::new(HashMap::new())));
        let relay = Arc::new(MemoryRelay::new(RELAY, None));
        start(relay.clone(), files.clone());
        tokio::time::sleep(Duration::from_millis(10)).await;

        let seller = people(&*files)[0];
        let offer = bot_offer(&relay, &seller);
        assert_eq!(offer.currency, XFK);
        assert_eq!((offer.amount_min, offer.amount_max), (1.0, 50.0));

        let mut me = Me::new(
            SecretKey::generate(),
            relay.clone(),
            seller,
            OfferTerms::from_offer(&offer, false),
        );
        me.offer = Some(offer.id);
        relay.publish(&[], ping(&me.sk, &seller));
        assert!(me.hear().await.text.starts_with("hello"));

        me.act(Action::Amount(20.0));
        assert_eq!(me.hear().await.action, Some(Action::Price(PRICE).to_tag()));
        me.act(Action::BitcoinMethod("lightning".into()));
        me.act(Action::PaymentMethod("Play money".into()));
        me.act(Action::BitcoinDetails("me@example.com".into()));
        me.hear().await;
        assert_eq!(me.deal().payment_details.as_deref(), Some(PAYMENT_DETAILS));
        assert_eq!(me.deal().sats(), Some(20_000));

        me.act(Action::Sent);
        assert_eq!(me.hear().await.action, Some(Action::Sent.to_tag()));
        assert!(me.deal().concluded());
        assert_eq!(me.hear().await.text, BYE);
    }

    #[tokio::test(start_paused = true)]
    async fn the_taker_takes_our_offer() {
        let files: Arc<dyn Files> = Arc::new(MemFiles(Mutex::new(HashMap::new())));
        let relay = Arc::new(MemoryRelay::new(RELAY, None));
        let taker = people(&*files)[2];
        let sk = SecretKey::generate();
        let mut pings = relay.subscribe(
            &[],
            Filter {
                kinds: Some(vec![KIND_PING]),
                tags: Some(vec![TagQuery("p".into(), vec![sk.pubkey().to_hex()])]),
                ..Default::default()
            },
        );
        pings.try_recv().unwrap();

        // we sell, so the taker gets the bitcoin and sends the fiat first
        let evt = NewOffer {
            side: Side::Sell,
            currency: XFK.into(),
            price_api: PRICE_API.into(),
            premium: 0.0,
            amount_min: 5.0,
            amount_max: 10.0,
            payment_methods: vec!["Play money".into()],
            bitcoin_methods: vec!["on-chain".into(), "liquid".into()],
            relays: vec![RELAY.into()],
            description: String::new(),
        }
        .to_event(&sk);
        let offer = Offer::from_event(&evt).unwrap();
        relay.publish(&[], evt);
        start(relay.clone(), files.clone());

        let ping = tokio::time::timeout(Duration::from_secs(20), pings.recv())
            .await
            .unwrap();
        assert!(matches!(ping, Some(Occurrence::Event(e)) if e.pubkey == taker));
        let mut me = Me::new(sk, relay.clone(), taker, OfferTerms::from_offer(&offer, true));
        assert!(me.hear().await.text.starts_with("hello"));
        me.hear().await;
        me.hear().await;
        let amount = me.deal().amount.unwrap();
        assert!((5.0..=10.0).contains(&amount));
        assert_eq!(me.deal().bitcoin_method.as_deref(), Some("on-chain"));

        me.act(Action::Price(PRICE));
        me.hear().await;
        assert_eq!(
            me.deal().bitcoin_details.as_deref(),
            Some(bitcoin_destination("on-chain"))
        );
        me.act(Action::PaymentDetails("my pretend bank".into()));
        me.hear().await;
        assert!(me.deal().taker_sent);
        me.act(Action::Sent);
        assert_eq!(me.hear().await.text, BYE);

        // it took the offer once
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert!(me.sub.try_recv().is_err());
    }
}

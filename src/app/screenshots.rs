//! screenshots for the readme and the user guide: the real ui, rendered offscreen by slint's
//! software renderer, over a made-up market, trades and reviews. nothing goes to the network.
//!
//! `just screenshots` (or `cargo run --features screenshots --bin screenshots -- <dir>`)

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ritualistic::{Event, EventTemplate, Kind, SecretKey, Tags, Timestamp};
use sha2::{Digest, Sha256};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{EventLoopProxy, Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, PhysicalSize};

use super::*;
use crate::store::Message;

/// a phone, in logical pixels, rendered at twice that
const PHONE: (u32, u32) = (420, 820);
/// wide enough for the deal panel to sit next to the chat
const DESKTOP: (u32, u32) = (1100, 720);
const SCALE: f32 = 2.0;

const RELAYS: &[&str] = &["wss://nostr.wine", "wss://nostr.land", "wss://nostr21.com"];

type Queue = Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;

/// a platform with one offscreen window, whose event loop is whatever `settle()` drains
struct Headless {
    window: Rc<MinimalSoftwareWindow>,
    queue: Queue,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.queue.clone())))
    }
}

struct Proxy(Queue);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        Ok(())
    }

    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), slint::EventLoopError> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

struct Shooter {
    window: Rc<MinimalSoftwareWindow>,
    queue: Queue,
    ui: AppWindow,
    out: PathBuf,
}

impl Shooter {
    /// run what the app sent to the event loop, until it has been quiet for a moment
    fn settle(&self) {
        let mut quiet_since = Instant::now();
        while quiet_since.elapsed() < Duration::from_millis(300) {
            let jobs = std::mem::take(&mut *self.queue.lock().unwrap());
            if !jobs.is_empty() {
                quiet_since = Instant::now();
            }
            for job in jobs {
                job();
            }
            slint::platform::update_timers_and_animations();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn resize(&self, (w, h): (u32, u32)) {
        self.window.set_size(PhysicalSize::new(
            (w as f32 * SCALE) as u32,
            (h as f32 * SCALE) as u32,
        ));
    }

    fn shoot(&self, name: &str) {
        self.settle();
        let shot = self
            .ui
            .window()
            .take_snapshot()
            .expect("failed to render the window");
        let path = self.out.join(format!("{name}.png"));
        let file = std::fs::File::create(&path).expect("failed to create the screenshot file");
        let mut png = png::Encoder::new(std::io::BufWriter::new(file), shot.width(), shot.height());
        png.set_color(png::ColorType::Rgba);
        png.set_depth(png::BitDepth::Eight);
        png.write_header()
            .and_then(|mut w| w.write_image_data(shot.as_bytes()))
            .expect("failed to write the screenshot");
        println!("{}", path.display());
    }
}

pub fn run(out: &Path) -> Result<(), slint::PlatformError> {
    let _ = std::fs::create_dir_all(out);
    let data =
        std::env::temp_dir().join(format!("bitcoin-smol-screenshots-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).expect("failed to create the demo data dir");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start tokio");
    let _guard = rt.enter();

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    window.dispatch_event(WindowEvent::ScaleFactorChanged {
        scale_factor: SCALE,
    });
    let queue = Queue::default();
    slint::platform::set_platform(Box::new(Headless {
        window: window.clone(),
        queue: queue.clone(),
    }))
    .expect("a platform was already set");

    let me = key("me");
    let config = data.join("config");
    std::fs::create_dir_all(&config).expect("failed to create the demo config dir");
    std::fs::write(config.join("key"), me.to_hex()).expect("failed to write the demo key");
    let mut store = Store::open(&data, &config);
    let demo = seed(&mut store, &me);
    let (ui, app) = build(store, true)?;
    *app.offers.lock().unwrap() = demo.offers;
    *app.prices.lock().unwrap() = demo.prices;
    app.render_offers();
    app.render_trades();
    app.render_people();
    ui.set_status(format!("{} offers from {} relays", demo.offer_count, RELAYS.len()).into());
    ui.show()?;

    let s = Shooter {
        window,
        queue,
        ui,
        out: out.to_path_buf(),
    };
    s.resize(PHONE);

    s.shoot("market");
    s.ui.invoke_open_offer(demo.featured_offer.into());
    s.shoot("offer");
    s.ui.set_page(2);
    s.shoot("new-offer");
    s.ui.set_page(3);
    s.shoot("trades");
    s.ui.invoke_open_trade(demo.trade_peer.clone().into());
    s.shoot("trade");
    s.resize(DESKTOP);
    s.shoot("trade-wide");
    s.resize(PHONE);
    s.ui.invoke_open_profile(demo.trade_peer.into());
    s.shoot("profile");
    s.ui.set_page(7);
    s.shoot("people");

    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

// ------------------------------------------------------------------------------ demo data

/// the same keys on every run, so the npubs in the screenshots don't change
fn key(name: &str) -> SecretKey {
    let hash = Sha256::digest(format!("bitcoin-smol screenshots {name}"));
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    hex.parse().expect("a sha256 is a valid secret key")
}

fn ago(minutes: u32) -> u32 {
    now().0 - minutes * 60
}

fn event(
    sk: &SecretKey,
    minutes_ago: u32,
    kind: Kind,
    tags: Vec<Vec<String>>,
    content: &str,
) -> Event {
    EventTemplate {
        created_at: Timestamp(ago(minutes_ago)),
        kind,
        tags: Tags(tags),
        content: content.into(),
    }
    .finalize(sk)
}

struct OfferSpec<'a> {
    side: &'a str,
    currency: &'a str,
    api: &'a str,
    premium: f64,
    amount: (f64, f64),
    payments: &'a [&'a str],
    bitcoin: &'a [&'a str],
    description: &'a str,
}

fn offer_event(sk: &SecretKey, minutes_ago: u32, o: &OfferSpec) -> Event {
    let created = ago(minutes_ago);
    let mut tags = vec![
        vec!["side".into(), o.side.into()],
        vec!["currency".into(), o.currency.into()],
        vec!["price_api".into(), o.api.into()],
        vec!["premium".into(), prices::format_premium(o.premium)],
        vec![
            "amount".into(),
            prices::format_amount(o.amount.0),
            prices::format_amount(o.amount.1),
        ],
    ];
    tags.extend(
        o.payments
            .iter()
            .map(|p| vec!["payment".into(), p.to_string()]),
    );
    tags.extend(
        o.bitcoin
            .iter()
            .map(|b| vec!["bitcoin".into(), b.to_string()]),
    );
    tags.extend(RELAYS.iter().map(|r| vec!["relay".into(), r.to_string()]));
    tags.push(vec![
        "expiration".into(),
        (created + OFFER_TTL_SECS).to_string(),
    ]);
    event(sk, minutes_ago, KIND_OFFER, tags, o.description)
}

fn review(
    author: &SecretKey,
    subject: &SecretKey,
    trade: Option<&str>,
    days_ago: u32,
    success: bool,
    comment: &str,
) -> Event {
    let mut tags = vec![vec!["p".into(), subject.pubkey().to_hex()]];
    tags.extend(trade.map(|t| vec![TRADE_REF.into(), t.to_string()]));
    event(
        author,
        days_ago * 24 * 60,
        if success {
            KIND_TRADE_SUCCESS
        } else {
            KIND_TRADE_FAILURE
        },
        tags,
        comment,
    )
}

/// put a trade where the app finds it when it starts: its ping and the rumors of the chat, and,
/// for a trade about an offer, the note about it
fn save_trade(
    store: &Store,
    me: &SecretKey,
    peer: &SecretKey,
    trade: &Trade,
    offer: Option<&Offer>,
) {
    let conv = Conversation::new(me, peer.pubkey(), &trade.trade_id);
    let (from, to) = if trade.started_by_me {
        (me, peer.pubkey())
    } else {
        (peer, me.pubkey())
    };
    store.save_event(
        &EventTemplate {
            created_at: Timestamp(trade.started_at),
            kind: KIND_PING,
            tags: Tags(vec![
                vec!["p".into(), to.to_hex()],
                vec!["trade".into(), trade.trade_id.clone()],
            ]),
            content: String::new(),
        }
        .finalize(from),
    );
    for m in &trade.messages {
        let (author, receiver) = if m.mine {
            (me.pubkey(), peer.pubkey())
        } else {
            (peer.pubkey(), me.pubkey())
        };
        let mut tags = vec![vec!["p".to_string(), receiver.to_hex()]];
        tags.extend(m.action.clone());
        let rumor = Rumor {
            id: String::new(),
            pubkey: author.to_hex(),
            created_at: m.created_at,
            kind: KIND_RUMOR.0,
            tags,
            content: m.text.clone(),
        };
        store.save_event(&conv.local_event(&rumor, &author));
    }
    if let Some((offer, terms)) = offer.zip(trade.offer.as_ref()) {
        store.save_event(&offer_mark_event(&me.pubkey(), &conv, offer, terms, trade));
    }
    if let Some(price) = trade.price_check {
        store.save_event(&trade_mark(
            &me.pubkey(),
            &peer.pubkey(),
            &trade.trade_id,
            MARK_PRICE_CHECK,
            vec![],
            &price.to_string(),
        ));
    }
}

/// a chat line: what someone typed, or one of the deal's structured actions
enum Line {
    Text(&'static str),
    Act(Action),
}

/// the messages of a trade as `me` sees them, with the text of each action as the app writes it
fn chat(terms: &OfferTerms, start_minutes_ago: u32, lines: Vec<(Role, Line)>) -> Vec<Message> {
    let me = terms.my_role();
    let mut deal = Deal::new(terms);
    lines
        .into_iter()
        .enumerate()
        .map(|(i, (who, line))| {
            let (text, action) = match line {
                Line::Text(t) => (t.to_string(), None),
                Line::Act(a) => {
                    let text = a.text(terms, who, &deal);
                    deal.apply(terms, who, &a);
                    (text, Some(a.to_tag()))
                }
            };
            Message {
                id: format!("demo-{start_minutes_ago}-{i}"),
                created_at: ago(start_minutes_ago) + i as u32 * 40,
                mine: who == me,
                text,
                action,
                ..Default::default()
            }
        })
        .collect()
}

struct Demo {
    offers: Vec<Offer>,
    offer_count: usize,
    prices: HashMap<(String, String), f64>,
    featured_offer: String,
    trade_peer: String,
}

fn seed(store: &mut Store, me: &SecretKey) -> Demo {
    let lucas = key("lucas");
    let ana = key("ana");
    let kofi = key("kofi");
    let priya = key("priya");
    let tomas = key("tomas");
    let wanjiru = key("wanjiru");
    let chidi = key("chidi");
    let sofia = key("sofia");
    let stranger = key("stranger");

    let prices: HashMap<(String, String), f64> = [
        ("USD", "coingecko", 112_480.0),
        ("EUR", "kraken", 96_310.0),
        ("ARS", "yadio", 162_400_000.0),
        ("BRL", "coingecko", 611_900.0),
        ("KES", "yadio", 14_520_000.0),
        ("NGN", "yadio", 172_800_000.0),
        ("INR", "coingecko", 9_870_000.0),
    ]
    .into_iter()
    .map(|(c, a, p)| ((c.to_string(), a.to_string()), p))
    .collect();

    let lucas_offer = OfferSpec {
        side: "sell",
        currency: "USD",
        api: "coingecko",
        premium: 2.0,
        amount: (5.0, 150.0),
        payments: &["Zelle", "Strike"],
        bitcoin: &["lightning", "liquid"],
        description: "",
    };
    let my_offer = OfferSpec {
        side: "buy",
        currency: "EUR",
        api: "kraken",
        premium: -1.0,
        amount: (20.0, 200.0),
        payments: &["SEPA", "Revolut"],
        bitcoin: &["lightning", "on-chain"],
        description: "",
    };
    let specs: Vec<(&SecretKey, u32, OfferSpec)> = vec![
        (&lucas, 4, lucas_offer),
        (me, 12, my_offer),
        (
            &tomas,
            18,
            OfferSpec {
                side: "buy",
                currency: "ARS",
                api: "yadio",
                premium: 3.0,
                amount: (50.0, 300.0),
                payments: &["Mercado Pago", "Bank transfer"],
                bitcoin: &["lightning"],
                description: "",
            },
        ),
        (
            &wanjiru,
            25,
            OfferSpec {
                side: "sell",
                currency: "KES",
                api: "yadio",
                premium: 1.5,
                amount: (5.0, 10_000.0),
                payments: &["M-Pesa"],
                bitcoin: &["lightning", "on-chain"],
                description: "",
            },
        ),
        (
            &ana,
            41,
            OfferSpec {
                side: "sell",
                currency: "BRL",
                api: "coingecko",
                premium: 1.0,
                amount: (50.0, 50.0),
                payments: &["PIX"],
                bitcoin: &["lightning", "liquid"],
                description: "",
            },
        ),
        (
            &priya,
            57,
            OfferSpec {
                side: "buy",
                currency: "INR",
                api: "coingecko",
                premium: 0.0,
                amount: (1_000.0, 20_000.0),
                payments: &["UPI"],
                bitcoin: &["lightning", "arkade"],
                description: "",
            },
        ),
        (
            &chidi,
            76,
            OfferSpec {
                side: "sell",
                currency: "NGN",
                api: "yadio",
                premium: 4.0,
                amount: (10_000.0, 500_000.0),
                payments: &["Bank transfer"],
                bitcoin: &["on-chain", "lightning"],
                description: "",
            },
        ),
        (
            &sofia,
            95,
            OfferSpec {
                side: "buy",
                currency: "EUR",
                api: "kraken",
                premium: -0.5,
                amount: (10.0, 100.0),
                payments: &["Bizum", "Instant SEPA"],
                bitcoin: &["lightning"],
                description: "",
            },
        ),
        (
            &stranger,
            130,
            OfferSpec {
                side: "sell",
                currency: "USD",
                api: "coingecko",
                premium: -3.0,
                amount: (50.0, 400.0),
                payments: &["Cash App", "PayPal Friends & Family"],
                bitcoin: &["on-chain"],
                description: "",
            },
        ),
    ];
    let events: Vec<Event> = specs
        .iter()
        .map(|(sk, minutes, spec)| offer_event(sk, *minutes, spec))
        .collect();
    let offers: Vec<Offer> = events.iter().filter_map(Offer::from_event).collect();
    let featured = offers[0].clone();
    let mine = offers[1].clone();

    let s = &mut store.state;
    s.app_relays = RELAYS.iter().map(|r| r.to_string()).collect();
    s.my_offers = vec![MyOffer {
        event: events[1].clone(),
        relays: s.app_relays.clone(),
    }];
    s.recent_offers = vec![OfferTemplate {
        side: "buy".into(),
        currency: "EUR".into(),
        price_api: "kraken".into(),
        premium: -1.0,
        amount: Some((20.0, 200.0)),
        payment_methods: vec!["SEPA".into(), "Revolut".into()],
        bitcoin_methods: vec!["lightning".into(), "on-chain".into()],
        relays: s.app_relays.clone(),
    }];
    s.labels.insert(lucas.pubkey().to_hex(), "lucas".into());

    // taking lucas's offer: everything agreed, our turn to send the dollars
    let terms = OfferTerms::from_offer(&featured, false);
    let price = prices::apply_premium(
        prices[&("USD".into(), "coingecko".into())],
        featured.premium,
    );
    use Role::{Maker, Taker};
    let messages = chat(
        &terms,
        9,
        vec![
            (Taker, Line::Text("hey lucas, still around?")),
            (Maker, Line::Text("yep, go ahead")),
            (Taker, Line::Act(Action::Amount(40.0))),
            (Maker, Line::Act(Action::Price(price.round()))),
            (Taker, Line::Act(Action::BitcoinMethod("lightning".into()))),
            (Taker, Line::Act(Action::PaymentMethod("Zelle".into()))),
            (
                Taker,
                Line::Act(Action::BitcoinDetails("satstacker@example.com".into())),
            ),
            (
                Maker,
                Line::Act(Action::PaymentDetails("lucas.m@example.com".into())),
            ),
        ],
    );
    let with_lucas = Trade {
        peer: lucas.pubkey().to_hex(),
        trade_id: "demo-lucas".into(),
        relays: featured.relays.clone(),
        offer_summary: offer_summary(&featured),
        offer_id: Some(featured.id.to_hex()),
        offer: Some(terms),
        price_check: Some(price),
        started_at: ago(9),
        started_by_me: true,
        messages,
        ..Default::default()
    };

    // someone answering our own offer
    let terms = OfferTerms::from_offer(&mine, true);
    let eur = prices::apply_premium(prices[&("EUR".into(), "kraken".into())], mine.premium);
    let messages = chat(
        &terms,
        3,
        vec![
            (Taker, Line::Text("hi! I can do 60 EUR via SEPA")),
            (Taker, Line::Act(Action::Amount(60.0))),
            (Maker, Line::Act(Action::Price(eur.round()))),
        ],
    );
    let with_kofi = Trade {
        peer: kofi.pubkey().to_hex(),
        trade_id: "demo-kofi".into(),
        relays: mine.relays.clone(),
        offer_summary: offer_summary(&mine),
        offer_id: Some(mine.id.to_hex()),
        offer: Some(terms),
        price_check: None,
        started_at: ago(3),
        started_by_me: false,
        messages,
        ..Default::default()
    };
    save_trade(store, me, &lucas, &with_lucas, Some(&featured));
    save_trade(store, me, &kofi, &with_kofi, Some(&mine));

    // past trades, and what we said about them
    let past = [
        (&ana, 2, true, "R$50 via PIX, quick and easy"),
        (&lucas, 6, true, "$20 via Zelle, smooth"),
        (&priya, 9, true, "₹2000 UPI, fast"),
        (&sofia, 15, true, "€15 bizum"),
        (&stranger, 20, false, "never paid, stopped answering"),
        (&lucas, 24, true, "$10 via Strike, first trade"),
        (&ana, 30, true, "R$30 PIX"),
    ];
    for (i, (sk, days, success, comment)) in past.into_iter().enumerate() {
        let trade = Trade {
            peer: sk.pubkey().to_hex(),
            trade_id: format!("demo-past-{i}"),
            started_at: ago(days * 24 * 60 + 30),
            started_by_me: true,
            ..Default::default()
        };
        save_trade(store, me, sk, &trade, None);
        store.save_event(&trade_mark_at(
            &me.pubkey(),
            &sk.pubkey(),
            &trade.trade_id,
            MARK_CONCLUDED,
            vec![],
            "",
            Timestamp(ago(days * 24 * 60)),
        ));
        store.save_event(&review(
            me,
            sk,
            Some(&trade.trade_id),
            days,
            success,
            comment,
        ));
    }

    // what the people we trust said about lucas
    for (author, days, success, comment) in [
        (&ana, 4, true, "$25 Strike, paid in a minute"),
        (&priya, 12, true, "$15 zelle"),
        (
            &sofia,
            40,
            false,
            "took two days to pay, but he did in the end",
        ),
        (&ana, 51, true, "$10 strike"),
    ] {
        store.save_event(&review(author, &lucas, None, days, success, comment));
    }
    store.persist();

    Demo {
        offer_count: offers.len(),
        offers,
        prices,
        featured_offer: featured.id.to_hex(),
        trade_peer: lucas.pubkey().to_npub(),
    }
}

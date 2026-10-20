//! the nostr events this app speaks. see README.md for the rationale.

use ritualistic::message_encryption::{decrypt, encrypt, generate_conversation_key};
use ritualistic::{Event, EventTemplate, ID, Kind, PubKey, SecretKey, Tags, Timestamp};

/// `Timestamp::now()`, which in ritualistic 0.2.3 asks `SystemTime` and so panics in the browser
pub fn now() -> Timestamp {
    #[cfg(target_arch = "wasm32")]
    return Timestamp((js_sys::Date::now() / 1000.0) as u32);
    #[cfg(not(target_arch = "wasm32"))]
    return Timestamp::now();
}

pub const KIND_OFFER: Kind = Kind(6012);
pub const KIND_DM: Kind = Kind(8021);
pub const KIND_PING: Kind = Kind(28022);
pub const KIND_DELETION: Kind = Kind(5);
pub const KIND_RUMOR: Kind = Kind(14);
pub const KIND_TRADE_FAILURE: Kind = Kind(8020);
pub const KIND_TRADE_SUCCESS: Kind = Kind(8022);
/// nip-65 relay list
pub const KIND_RELAY_LIST: Kind = Kind(10002);

/// offers expire after this, and clients only look this far back
pub const OFFER_TTL_SECS: u32 = 3 * 60 * 60;

/// dms are stored by relays, but expire (nip-40) after this
pub const DM_TTL_SECS: u32 = 30 * 24 * 60 * 60;

/// how the bitcoin side of the trade can be settled
pub const BITCOIN_METHODS: &[&str] = &[
    "on-chain",
    "rootstock",
    "lightning",
    "cassis",
    "arkade",
    "liquid",
];

/// bitcoin methods the new offer form starts with when there is no history
pub const DEFAULT_BITCOIN_METHODS: &[&str] = &["on-chain", "lightning", "arkade"];

/// fiat payment methods and the currencies they are common for, `*` meaning all of them.
/// the new offer form shows the ones for the chosen currency first and hides the rest.
pub const PAYMENT_METHODS: &[(&str, &[&str])] = &[
    ("Bank transfer", &["*"]),
    ("Play money", &["XFK"]),
    ("Cash F2F", &["*"]),
    (
        "Revolut",
        &["USD", "EUR", "GBP", "CHF", "AUD", "CAD", "JPY"],
    ),
    (
        "Wise",
        &[
            "USD", "EUR", "GBP", "CHF", "AUD", "CAD", "JPY", "BRL", "MXN", "INR",
        ],
    ),
    ("Zelle", &["USD"]),
    ("Strike", &["USD", "EUR", "GBP"]),
    ("Cash App", &["USD"]),
    ("Venmo", &["USD"]),
    (
        "PayPal Friends & Family",
        &["USD", "EUR", "GBP", "CAD", "AUD", "CHF", "JPY"],
    ),
    ("SEPA", &["EUR"]),
    ("Instant SEPA", &["EUR"]),
    ("Faster Payments", &["GBP"]),
    ("Interac e-Transfer", &["CAD"]),
    ("Bizum", &["EUR"]),
    ("MB WAY", &["EUR"]),
    ("N26", &["EUR"]),
    ("Lydia", &["EUR"]),
    ("Satispay", &["EUR"]),
    ("MobilePay", &["EUR"]),
    ("Swish", &["SEK"]),
    ("Vipps", &["NOK"]),
    ("TWINT", &["CHF"]),
    ("PIX", &["BRL"]),
    ("Nubank", &["BRL", "MXN", "COP"]),
    ("Mercado Pago", &["ARS", "BRL", "MXN", "CLP"]),
    ("Pago Movil", &["VES"]),
    ("Nequi", &["COP"]),
    ("Daviplata", &["COP"]),
    ("Yape", &["PEN"]),
    ("Plin", &["PEN"]),
    ("SINPE Movil", &["CRC"]),
    ("M-Pesa", &["KES"]),
    ("UPI", &["INR"]),
    ("Paytm", &["INR"]),
    ("Advcash", &[]),
    ("Uphold", &[]),
    ("Payoneer", &[]),
    ("Amazon GiftCard", &[]),
    ("Google Play Gift Code", &[]),
    ("Steam Wallet Gift Card", &[]),
];

/// (common for this currency, all the others)
pub fn payment_methods_for(currency: &str) -> (Vec<&'static str>, Vec<&'static str>) {
    let mut common = vec![];
    let mut rest = vec![];
    for (name, curs) in PAYMENT_METHODS {
        if curs.contains(&"*") || curs.contains(&currency) {
            common.push(*name);
        } else {
            rest.push(*name);
        }
    }
    (common, rest)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// the maker is selling bitcoin for fiat
    Sell,
    /// the maker is buying bitcoin with fiat
    Buy,
}

impl Side {
    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Sell => "sell",
            Side::Buy => "buy",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Offer {
    pub id: ID,
    pub maker: PubKey,
    pub created_at: Timestamp,
    pub expiration: Timestamp,
    pub side: Side,
    pub currency: String,
    pub price_api: String,
    /// percentage over (or under, if negative) the market price
    pub premium: f64,
    /// how much fiat, in `currency`, the maker trades: anything from min to max
    pub amount_min: f64,
    pub amount_max: f64,
    pub payment_methods: Vec<String>,
    pub bitcoin_methods: Vec<String>,
    pub relays: Vec<String>,
    pub description: String,
}

pub struct NewOffer {
    pub side: Side,
    pub currency: String,
    pub price_api: String,
    pub premium: f64,
    pub amount_min: f64,
    pub amount_max: f64,
    pub payment_methods: Vec<String>,
    pub bitcoin_methods: Vec<String>,
    pub relays: Vec<String>,
    pub description: String,
}

impl NewOffer {
    pub fn to_event(&self, sk: &SecretKey) -> Event {
        let now = now();
        let mut tags = vec![
            vec!["side".into(), self.side.as_str().into()],
            vec!["currency".into(), self.currency.to_uppercase()],
            vec!["price_api".into(), self.price_api.clone()],
        ];
        if self.premium != 0.0 {
            tags.push(vec![
                "premium".into(),
                crate::prices::format_premium(self.premium),
            ]);
        }
        tags.push(vec![
            "amount".into(),
            crate::prices::format_amount(self.amount_min),
            crate::prices::format_amount(self.amount_max),
        ]);
        for pm in &self.payment_methods {
            tags.push(vec!["payment".into(), pm.clone()]);
        }
        for bm in &self.bitcoin_methods {
            tags.push(vec!["bitcoin".into(), bm.clone()]);
        }
        for r in &self.relays {
            tags.push(vec!["relay".into(), r.clone()]);
        }
        tags.push(vec![
            "expiration".into(),
            (now.0 + OFFER_TTL_SECS).to_string(),
        ]);

        EventTemplate {
            created_at: now,
            kind: KIND_OFFER,
            tags: Tags(tags),
            content: self.description.clone(),
        }
        .finalize(sk)
    }
}

impl Offer {
    pub fn from_event(evt: &Event) -> Option<Self> {
        if evt.kind != KIND_OFFER {
            return None;
        }
        let tag = |k: &str| evt.tags.find(k).map(|t| t[1].clone());
        let expiration = Timestamp(tag("expiration")?.parse().ok()?);
        // never accept offers that claim to live longer than allowed or that are already dead
        let now = now();
        if expiration <= now || expiration.0 > evt.created_at.0 + OFFER_TTL_SECS + 60 {
            return None;
        }
        let side = match tag("side")?.as_str() {
            "sell" => Side::Sell,
            "buy" => Side::Buy,
            _ => return None,
        };
        let relays: Vec<String> = evt
            .tags
            .find_all("relay")
            .filter_map(|t| ritualistic::normalize_url(&t[1]).ok())
            .map(|u| u.to_string())
            .collect();
        if relays.is_empty() {
            return None;
        }
        let amount = evt.tags.find("amount")?;
        let amount_min = crate::prices::parse_amount(&amount[1])?;
        // a single value is a fixed amount
        let amount_max = match amount.get(2) {
            Some(max) => crate::prices::parse_amount(max)?,
            None => amount_min,
        };
        if amount_min > amount_max {
            return None;
        }

        Some(Offer {
            id: evt.id,
            maker: evt.pubkey,
            created_at: evt.created_at,
            expiration,
            side,
            currency: tag("currency")?.to_uppercase(),
            price_api: tag("price_api")?,
            premium: crate::prices::parse_premium(&tag("premium").unwrap_or_default())?,
            amount_min,
            amount_max,
            payment_methods: evt.tags.find_all("payment").map(|t| t[1].clone()).collect(),
            bitcoin_methods: evt.tags.find_all("bitcoin").map(|t| t[1].clone()).collect(),
            relays,
            description: evt.content.clone(),
        })
    }
}

/// the shared state between two parties: the nip44 conversation key doubles as a secret key
/// that either of them can sign with.
#[derive(Clone)]
pub struct Conversation {
    pub peer: PubKey,
    /// what tells this trade from any other one with the same peer, empty for the legacy single
    /// conversation per pair of keys
    pub trade: String,
    pub key: [u8; 32],
    pub signer: SecretKey,
}

impl Conversation {
    /// `trade` is a string the taker makes up for each trade and sends along in the ping. it goes
    /// into the key so that two trades between the same two parties never share a conversation.
    pub fn new(me: &SecretKey, peer: PubKey, trade: &str) -> Self {
        let shared = generate_conversation_key(&peer, me);
        let key = if trade.is_empty() {
            shared
        } else {
            sha256(&[b"smol-trade:".as_slice(), &shared, trade.as_bytes()].concat())
        };
        let signer = SecretKey::from_bytes(key)
            .expect("a conversation key outside the curve order is astronomically unlikely");
        Self {
            peer,
            trade: trade.to_string(),
            key,
            signer,
        }
    }

    /// the pubkey under which every message between the two parties is published.
    /// filtering by it (or not) is how one opts in (or blocks) a peer.
    pub fn pubkey(&self) -> PubKey {
        self.signer.pubkey()
    }

    /// wrap a message in a kind:14 rumor, encrypt it and sign the wrapper with the conversation key.
    /// `extra` is one more tag for the rumor, like the action of a structured trade message.
    /// returns the rumor id (to be remembered as "mine") and the event to publish.
    pub fn wrap(
        &self,
        me: &PubKey,
        text: &str,
        offer: Option<ID>,
        extra: Option<Vec<String>>,
    ) -> (Rumor, Event) {
        let mut tags = vec![vec!["p".to_string(), self.peer.to_hex()]];
        if let Some(offer) = offer {
            tags.push(vec!["e".into(), offer.to_hex()]);
        }
        tags.extend(extra);
        let mut rumor = Rumor {
            id: String::new(),
            pubkey: me.to_hex(),
            created_at: now().0,
            kind: KIND_RUMOR.0,
            tags,
            content: text.to_string(),
        };
        rumor.id = rumor.compute_id();

        let ciphertext = encrypt(
            &serde_json::to_string(&rumor).expect("rumor serializes"),
            &self.key,
            None,
        )
        .expect("message size is bounded by the ui");

        let now = now();
        let evt = EventTemplate {
            created_at: now,
            kind: KIND_DM,
            tags: Tags(vec![vec![
                "expiration".into(),
                (now.0 + DM_TTL_SECS).to_string(),
            ]]),
            content: ciphertext,
        }
        .finalize(&self.signer);

        (rumor, evt)
    }

    pub fn unwrap(&self, evt: &Event) -> Option<Rumor> {
        if evt.kind != KIND_DM || evt.pubkey != self.pubkey() || !evt.verify_signature() {
            return None;
        }
        let expiration = evt
            .tags
            .find("expiration")
            .and_then(|t| t[1].parse::<u32>().ok());
        if expiration.is_some_and(|exp| exp <= now().0) {
            return None;
        }
        let plain = decrypt(&evt.content, &self.key).ok()?;
        let rumor: Rumor = serde_json::from_str(&plain).ok()?;
        (rumor.kind == KIND_RUMOR.0 && rumor.id == rumor.compute_id()).then_some(rumor)
    }
}

/// an unsigned event, as in nip-59
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Rumor {
    pub id: String,
    pub pubkey: String,
    pub created_at: u32,
    pub kind: u16,
    pub tags: Vec<Vec<String>>,
    pub content: String,
}

impl Rumor {
    fn compute_id(&self) -> String {
        use std::fmt::Write;
        let ser = serde_json::json!([
            0,
            self.pubkey,
            self.created_at,
            self.kind,
            self.tags,
            self.content
        ])
        .to_string();
        let hash = sha256(ser.as_bytes());
        hash.iter().fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }
}

/// the longest trade string we accept in a ping
const MAX_TRADE_ID: usize = 64;

/// a new string to tell a trade apart from the others with the same peer
pub fn new_trade_id() -> String {
    SecretKey::generate().pubkey().to_hex()[..32].to_string()
}

/// the trade string of a ping, empty if it has none or a bad one (pings of older versions)
pub fn ping_trade_id(evt: &Event) -> String {
    evt.tags
        .find("trade")
        .map(|t| t[1].clone())
        .filter(|id| id.len() <= MAX_TRADE_ID)
        .unwrap_or_default()
}

/// "there is a DM for you from me", about the trade `trade` (see `Conversation::new`)
pub fn ping(me: &SecretKey, peer: &PubKey, trade: &str) -> Event {
    let mut tags = vec![vec!["p".into(), peer.to_hex()]];
    if !trade.is_empty() {
        tags.push(vec!["trade".into(), trade.to_string()]);
    }
    EventTemplate {
        created_at: now(),
        kind: KIND_PING,
        tags: Tags(tags),
        content: String::new(),
    }
    .finalize(me)
}

/// a public statement that a trade with `peer` went well (8022) or not (8020)
/// nip-09: ask relays to drop one of our events
pub fn deletion(me: &SecretKey, id: &ID, kind: Kind) -> Event {
    EventTemplate {
        created_at: now(),
        kind: KIND_DELETION,
        tags: Tags(vec![
            vec!["e".into(), id.to_hex()],
            vec!["k".into(), kind.0.to_string()],
        ]),
        content: String::new(),
    }
    .finalize(me)
}

pub fn trade_review(me: &SecretKey, peer: &PubKey, success: bool, comment: &str) -> Event {
    EventTemplate {
        created_at: now(),
        kind: if success {
            KIND_TRADE_SUCCESS
        } else {
            KIND_TRADE_FAILURE
        },
        tags: Tags(vec![vec!["p".into(), peer.to_hex()]]),
        content: comment.to_string(),
    }
    .finalize(me)
}

/// the relays someone writes to, from their nip-65 relay list: `r` tags marked "write" or not
/// marked at all
pub fn write_relays(relay_list: &Event) -> Vec<String> {
    marked_relays(relay_list, "write")
}

/// the relays someone reads from, from their nip-65 relay list: `r` tags marked "read" or not
/// marked at all
pub fn read_relays(relay_list: &Event) -> Vec<String> {
    marked_relays(relay_list, "read")
}

fn marked_relays(relay_list: &Event, wanted: &str) -> Vec<String> {
    let mut relays: Vec<String> = vec![];
    for tag in relay_list.tags.find_all("r") {
        if tag.get(2).is_some_and(|marker| marker != wanted) {
            continue;
        }
        if let Ok(url) = ritualistic::normalize_url(&tag[1]) {
            let url = url.to_string();
            if !relays.contains(&url) {
                relays.push(url);
            }
        }
    }
    relays
}

/// our nip-65 relay list, with `relays` as write relays
pub fn relay_list(me: &SecretKey, relays: &[String]) -> Event {
    EventTemplate {
        created_at: now(),
        kind: KIND_RELAY_LIST,
        tags: Tags(
            relays
                .iter()
                .map(|r| vec!["r".into(), r.clone(), "write".into()])
                .collect(),
        ),
        content: String::new(),
    }
    .finalize(me)
}

fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(data).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_relays() {
        let evt = EventTemplate {
            created_at: now(),
            kind: KIND_RELAY_LIST,
            tags: Tags(vec![
                vec!["r".into(), "wss://both.example".into()],
                vec!["r".into(), "wss://write.example".into(), "write".into()],
                vec!["r".into(), "wss://read.example".into(), "read".into()],
                vec!["r".into(), "wss://both.example/".into()],
                vec!["r".into(), "not a url".into()],
            ]),
            content: String::new(),
        }
        .finalize(&SecretKey::generate());
        assert_eq!(
            super::write_relays(&evt),
            vec!["wss://both.example/", "wss://write.example/"]
        );

        let ours = relay_list(&SecretKey::generate(), &["wss://a.example/".into()]);
        assert!(ours.verify_signature());
        assert_eq!(super::write_relays(&ours), vec!["wss://a.example/"]);
    }

    #[test]
    fn both_sides_derive_the_same_conversation() {
        let a = SecretKey::generate();
        let b = SecretKey::generate();
        let ca = Conversation::new(&a, b.pubkey(), "t");
        let cb = Conversation::new(&b, a.pubkey(), "t");
        assert_eq!(ca.pubkey(), cb.pubkey());

        let (rumor, evt) = ca.wrap(&a.pubkey(), "hello", None, None);
        assert_eq!(evt.pubkey, ca.pubkey());
        let exp: u32 = evt.tags.find("expiration").unwrap()[1].parse().unwrap();
        assert_eq!(exp, evt.created_at.0 + DM_TTL_SECS);
        let got = cb.unwrap(&evt).unwrap();
        assert_eq!(got.id, rumor.id);
        assert_eq!(got.content, "hello");

        // a third party can't read or forge it
        let c = SecretKey::generate();
        assert!(
            Conversation::new(&c, a.pubkey(), "t")
                .unwrap(&evt)
                .is_none()
        );
    }

    #[test]
    fn offer_roundtrip() {
        let sk = SecretKey::generate();
        let evt = NewOffer {
            side: Side::Sell,
            currency: "usd".into(),
            price_api: "coingecko".into(),
            premium: 1.5,
            amount_min: 20.0,
            amount_max: 150.5,
            payment_methods: vec!["Strike".into(), "Zelle".into()],
            bitcoin_methods: vec!["lightning".into(), "liquid".into()],
            relays: vec!["wss://nostr.wine".into()],
            description: "evenings only".into(),
        }
        .to_event(&sk);
        assert!(evt.verify_signature());
        let o = Offer::from_event(&evt).unwrap();
        assert_eq!(o.currency, "USD");
        assert_eq!(o.premium, 1.5);
        assert_eq!((o.amount_min, o.amount_max), (20.0, 150.5));
        assert_eq!(o.payment_methods.len(), 2);
        assert_eq!(o.bitcoin_methods, vec!["lightning", "liquid"]);
        assert_eq!(o.relays, vec!["wss://nostr.wine/".to_string()]);
    }
}

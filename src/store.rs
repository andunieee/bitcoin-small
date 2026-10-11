//! everything this app keeps on the device: the key, the json state (settings, open offers,
//! ongoing trades), the history of past trades and trade reports, each in a redb database.
//!
//! the key and the json state are small files: a directory on disk, or localStorage in the
//! browser. the redb databases live on disk, or in the browser's origin private file system.

use std::collections::{BTreeMap, HashMap, HashSet};

use ritualistic::database::EventDatabase;
use ritualistic::{Event, Filter, PubKey, RedbEventDatabase, SecretKey, TagQuery};

use crate::deal::OfferTerms;
use crate::form::OfferTemplate;
use crate::history::History;
use crate::protocol::{KIND_TRADE_FAILURE, KIND_TRADE_SUCCESS};

pub const DEFAULT_RELAYS: &[&str] = &[
    "wss://nostr.land",
    "wss://nostr21.com",
    "wss://nostr.wine",
    "wss://eden.nostr.land",
    "wss://atlas.nostr.land",
    "wss://relay.nostrplebs.com",
    "wss://nostr.plebchain.org",
];

/// how many of our own reports about someone are enough to not ask anyone else
pub const ENOUGH_OWN_REPORTS: usize = 20;
/// how many of our most-traded-with peers we ask about someone
pub const TRUSTED_PEERS_TO_ASK: usize = 10;

/// where the small files go. each write replaces the whole file at once.
pub trait Files: Send + Sync {
    fn read(&self, name: &str) -> Option<String>;
    fn write(&self, name: &str, contents: &str) -> std::io::Result<()>;
}

/// files in a directory on disk
#[cfg(not(target_arch = "wasm32"))]
pub struct Dir(pub std::path::PathBuf);

#[cfg(not(target_arch = "wasm32"))]
impl Files for Dir {
    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.0.join(name)).ok()
    }

    fn write(&self, name: &str, contents: &str) -> std::io::Result<()> {
        let tmp = self.0.join(format!("{name}.tmp"));
        std::fs::write(&tmp, contents)?;
        std::fs::rename(&tmp, self.0.join(name))
    }
}

/// the browser's localStorage, every name under one prefix
#[cfg(target_arch = "wasm32")]
pub struct LocalStorage;

#[cfg(target_arch = "wasm32")]
impl LocalStorage {
    fn storage() -> std::io::Result<web_sys::Storage> {
        web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .ok_or_else(|| std::io::Error::other("localStorage is not available"))
    }

    fn key(name: &str) -> String {
        format!("bitcoin-smol/{name}")
    }
}

#[cfg(target_arch = "wasm32")]
impl Files for LocalStorage {
    fn read(&self, name: &str) -> Option<String> {
        Self::storage().ok()?.get_item(&Self::key(name)).ok()?
    }

    fn write(&self, name: &str, contents: &str) -> std::io::Result<()> {
        Self::storage()?
            .set_item(&Self::key(name), contents)
            .map_err(|e| std::io::Error::other(format!("{e:?}")))
    }
}

pub fn load_or_create_key(files: &dyn Files) -> SecretKey {
    if let Some(hex) = files.read("key")
        && let Ok(sk) = hex.trim().parse::<SecretKey>()
    {
        return sk;
    }
    let sk = SecretKey::generate();
    if let Err(err) = files.write("key", &sk.to_hex()) {
        log::error!("failed to save the key: {err}");
    }
    sk
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub id: String,
    pub created_at: u32,
    pub mine: bool,
    pub text: String,
    /// the `["trade", …]` tag of a structured message, see `deal.rs`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Vec<String>>,
}

/// an ongoing trade with someone: one per peer, as the conversation key is per pair of keys
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Trade {
    /// relays of the offer this trade started from, where both sides publish and listen
    pub relays: Vec<String>,
    pub offer_summary: String,
    #[serde(default)]
    pub offer_id: Option<String>,
    /// the terms of that offer. the maker learns which offer it is from the taker's messages.
    #[serde(default)]
    pub offer: Option<OfferTerms>,
    /// messages from before this are left out, they belong to an earlier trade with the same peer
    #[serde(default)]
    pub started_at: u32,
    #[serde(default)]
    pub started_by_me: bool,
    pub messages: Vec<Message>,
}

/// an offer we published, until it expires or we close it
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MyOffer {
    pub event: Event,
    /// relays that accepted it, where the deletion goes when we close it
    pub relays: Vec<String>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct State {
    /// relays currently in use
    pub app_relays: Vec<String>,
    /// relays shown in the relay menu, enabled or not
    pub known_relays: Vec<String>,
    /// relays of offers we made recently, where we listen for pings
    #[serde(default)]
    pub ping_relays: Vec<String>,
    /// ids of the rumors we created, the only proof of who said what in a conversation
    #[serde(default)]
    pub my_rumors: HashSet<String>,
    /// peer pubkey hex -> ongoing trade. past ones go to the history database
    #[serde(default, alias = "conversations")]
    pub trades: BTreeMap<String, Trade>,
    #[serde(default)]
    pub my_offers: Vec<MyOffer>,
    #[serde(default)]
    pub blocked: HashSet<String>,
    /// peer pubkey hex -> a name for them that only we see, never published
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// what we filled in for our last offers, most recent first, to fill the next one
    #[serde(default)]
    pub recent_offers: Vec<OfferTemplate>,
}

pub struct Store {
    pub files: Box<dyn Files>,
    pub state: State,
    pub db: RedbEventDatabase,
    pub history: History,
}

impl Store {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(dir: &std::path::Path) -> Self {
        let db = RedbEventDatabase::open(dir.join("events.redb")).expect("failed to open database");
        let history = redb::Database::create(dir.join("history.redb"))
            .map_err(|e| e.to_string())
            .and_then(History::from_database)
            .expect("failed to open the trade history");
        Self::new(Box::new(Dir(dir.to_path_buf())), db, history)
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn open() -> Self {
        let db = RedbEventDatabase::open_opfs("events.redb")
            .await
            .expect("failed to open database");
        let history = match ritualistic::OpfsBackend::open("history.redb").await {
            Ok(backend) => redb::Database::builder()
                .create_with_backend(backend)
                .map_err(|e| e.to_string())
                .and_then(History::from_database),
            Err(err) => Err(err.to_string()),
        }
        .expect("failed to open the trade history");
        Self::new(Box::new(LocalStorage), db, history)
    }

    fn new(files: Box<dyn Files>, db: RedbEventDatabase, history: History) -> Self {
        let state = files
            .read("state.json")
            .and_then(|s| serde_json::from_str::<State>(&s).ok())
            .unwrap_or_else(|| State {
                app_relays: DEFAULT_RELAYS.iter().map(|s| s.to_string()).collect(),
                known_relays: DEFAULT_RELAYS.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            });
        Self {
            files,
            state,
            db,
            history,
        }
    }

    /// save `new` as our key. the old one stays next to it as `key.<pubkey>.old`, just in case
    pub fn replace_key(&self, old: &SecretKey, new: &SecretKey) -> std::io::Result<()> {
        let backup = format!("key.{}.old", old.pubkey().to_hex());
        self.files.write(&backup, &old.to_hex())?;
        self.files.write("key", &new.to_hex())
    }

    pub fn persist(&self) {
        let res = serde_json::to_string(&self.state)
            .map_err(std::io::Error::other)
            .and_then(|json| self.files.write("state.json", &json));
        if let Err(err) = res {
            log::error!("failed to persist state: {err}");
        }
    }

    pub fn save_event(&self, evt: &Event) {
        match self.db.save_event(evt) {
            Ok(()) | Err(ritualistic::database::DatabaseError::DuplicateEvent) => {}
            Err(err) => log::warn!("failed to save event {}: {err}", evt.id),
        }
    }

    fn query(&self, filter: Filter) -> Vec<Event> {
        match self.db.query_events(&mut [filter]) {
            Ok(results) => results
                .filter_map(|r| rkyv::deserialize::<Event, rkyv::rancor::Error>(&*r).ok())
                .collect(),
            Err(err) => {
                log::warn!("local query failed: {err}");
                vec![]
            }
        }
    }

    /// trade reports (8020/8022) by `authors` about `subject`
    pub fn reports_about(&self, authors: &[PubKey], subject: &PubKey) -> Vec<Event> {
        self.query(Filter {
            kinds: Some(vec![KIND_TRADE_FAILURE, KIND_TRADE_SUCCESS]),
            authors: Some(authors.to_vec()),
            tags: Some(vec![TagQuery("p".into(), vec![subject.to_hex()])]),
            ..Default::default()
        })
    }

    /// all trade reports (8020/8022) written by `author`
    pub fn reports_by(&self, author: &PubKey) -> Vec<Event> {
        self.query(Filter {
            kinds: Some(vec![KIND_TRADE_FAILURE, KIND_TRADE_SUCCESS]),
            authors: Some(vec![*author]),
            ..Default::default()
        })
    }

    /// the people we have reported the most successful trades with, best first
    pub fn top_trade_partners(&self, me: &PubKey, n: usize) -> Vec<PubKey> {
        let mine = self.query(Filter {
            kinds: Some(vec![KIND_TRADE_SUCCESS]),
            authors: Some(vec![*me]),
            ..Default::default()
        });
        let mut counts: HashMap<PubKey, usize> = HashMap::new();
        for evt in &mine {
            if let Some(pk) = evt.tags.find("p").and_then(|t| t[1].parse::<PubKey>().ok())
                && pk != *me
            {
                *counts.entry(pk).or_default() += 1;
            }
        }
        let mut ranked: Vec<_> = counts.into_iter().collect();
        ranked.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        ranked.into_iter().take(n).map(|(pk, _)| pk).collect()
    }
}

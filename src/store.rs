//! everything this app keeps on the device: the key, the json state (settings, open offers,
//! ongoing trades), the history of past trades and trade reports, each in a redb database.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use ritualistic::database::EventDatabase;
use ritualistic::{Event, Filter, PubKey, RedbEventDatabase, SecretKey, TagQuery};

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

pub fn load_or_create_key(dir: &Path) -> SecretKey {
    let path = dir.join("key");
    if let Ok(hex) = std::fs::read_to_string(&path)
        && let Ok(sk) = hex.trim().parse::<SecretKey>()
    {
        return sk;
    }
    let sk = SecretKey::generate();
    if let Err(err) = std::fs::write(&path, sk.to_hex()) {
        log::error!("failed to save the key at {}: {err}", path.display());
    }
    sk
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub id: String,
    pub created_at: u32,
    pub mine: bool,
    pub text: String,
}

/// an ongoing trade with someone: one per peer, as the conversation key is per pair of keys
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Trade {
    /// relays of the offer this trade started from, where both sides publish and listen
    pub relays: Vec<String>,
    pub offer_summary: String,
    #[serde(default)]
    pub offer_id: Option<String>,
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
    dir: PathBuf,
    pub state: State,
    pub db: RedbEventDatabase,
    pub history: History,
}

impl Store {
    pub fn open(dir: &Path) -> Self {
        let state = std::fs::read(dir.join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<State>(&b).ok())
            .unwrap_or_else(|| State {
                app_relays: DEFAULT_RELAYS.iter().map(|s| s.to_string()).collect(),
                known_relays: DEFAULT_RELAYS.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            });
        let db = RedbEventDatabase::open(dir.join("events.redb")).expect("failed to open database");
        let history = History::open(dir).expect("failed to open the trade history");
        Self {
            dir: dir.to_path_buf(),
            state,
            db,
            history,
        }
    }

    /// save `new` as our key. the old one stays next to it as `key.<pubkey>.old`, just in case
    pub fn replace_key(&self, old: &SecretKey, new: &SecretKey) -> std::io::Result<()> {
        let path = self.dir.join("key");
        let backup = self.dir.join(format!("key.{}.old", old.pubkey().to_hex()));
        std::fs::write(&backup, old.to_hex())?;
        let tmp = self.dir.join("key.tmp");
        std::fs::write(&tmp, new.to_hex())?;
        std::fs::rename(&tmp, &path)
    }

    pub fn persist(&self) {
        let tmp = self.dir.join("state.json.tmp");
        let res = serde_json::to_vec(&self.state)
            .map_err(std::io::Error::other)
            .and_then(|b| std::fs::write(&tmp, b))
            .and_then(|_| std::fs::rename(&tmp, self.dir.join("state.json")));
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

//! everything this app keeps on the device: the key, settings, conversations and trade reports.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use ritualistic::database::EventDatabase;
use ritualistic::{Event, Filter, PubKey, RedbEventDatabase, SecretKey, TagQuery};

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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ConversationState {
    /// relays of the offer this conversation started from, where both sides publish and listen
    pub relays: Vec<String>,
    pub offer_summary: String,
    #[serde(default)]
    pub offer_id: Option<String>,
    pub messages: Vec<Message>,
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
    /// peer pubkey hex -> conversation
    #[serde(default)]
    pub conversations: BTreeMap<String, ConversationState>,
    #[serde(default)]
    pub blocked: HashSet<String>,
    /// peer pubkey hex -> a name for them that only we see, never published
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

pub struct Store {
    dir: PathBuf,
    pub state: State,
    pub db: RedbEventDatabase,
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
        Self {
            dir: dir.to_path_buf(),
            state,
            db,
        }
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

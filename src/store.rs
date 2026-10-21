//! everything this app keeps on the device: the key, the json state (settings, open offers,
//! ongoing trades), the history of past trades and trade reviews, each in a redb database.
//!
//! the key and the json state are small files: a directory on disk, or localStorage in the
//! browser. the redb databases live on disk, or in the browser's origin private file system.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ritualistic::database::EventDatabase;
use ritualistic::{Event, Filter, PubKey, RedbEventDatabase, SecretKey, TagQuery};

use crate::deal::OfferTerms;
use crate::form::OfferTemplate;
use crate::history::History;
use crate::protocol::{KIND_TRADE_FAILURE, KIND_TRADE_SUCCESS};

#[cfg(not(debug_assertions))]
pub const DEFAULT_RELAYS: &[&str] = &["wss://nostr.wine"];
#[cfg(debug_assertions)]
pub const DEFAULT_RELAYS: &[&str] = &[
    "wss://nostr.wine",
    "wss://nos.lol",
    "wss://relay.primal.net",
];

/// our nip-65 write relays, when we make our first offer and have no relay list yet
pub const DEFAULT_WRITE_RELAYS: &[&str] = &["wss://relay.primal.net", "wss://nos.lol"];

/// how many of our own reviews about someone are enough to not ask anyone else
pub const ENOUGH_OWN_REVIEWS: usize = 20;
/// how many of our most-traded-with peers we ask about someone
pub const TRUSTED_PEERS_TO_ASK: usize = 10;
/// a successful last trade makes someone trusted for this long
pub const TRUST_SUCCESS_SECS: u32 = 183 * 24 * 60 * 60;

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

/// the key used to live in the data directory, and so did its backups. move them to the config
/// directory, unless it already has a key.
#[cfg(not(target_arch = "wasm32"))]
fn move_keys(data_dir: &std::path::Path, config_dir: &std::path::Path) {
    if data_dir == config_dir || config_dir.join("key").exists() || !data_dir.join("key").exists() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let ours = name == "key" || (name.starts_with("key.") && name.ends_with(".old"));
        if !ours {
            continue;
        }
        // not a rename, they may be on different file systems
        let from = entry.path();
        let to = config_dir.join(name);
        match std::fs::copy(&from, &to).and_then(|_| std::fs::remove_file(&from)) {
            Ok(()) => {}
            Err(err) => log::error!("failed to move {name} to {}: {err}", config_dir.display()),
        }
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

/// our key is in "key" of the config files. the bots of the practice market (see `xfk.rs`) keep
/// theirs in the data files.
pub fn load_or_create_key(files: &dyn Files, name: &str) -> SecretKey {
    if let Some(hex) = files.read(name) {
        if let Ok(sk) = hex.trim().parse::<SecretKey>() {
            return sk;
        }
        // never replace what we can't read without keeping it
        log::error!("the key {name} is unreadable, kept as {name}.bad");
        if let Err(err) = files.write(&format!("{name}.bad"), &hex) {
            log::error!("failed to keep the key {name}: {err}");
        }
    }
    let sk = SecretKey::generate();
    if let Err(err) = files.write(name, &sk.to_hex()) {
        log::error!("failed to save the key {name}: {err}");
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
    /// the wrapped event as it went to (or came from) the relays, as json
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub event: String,
    /// for ours, what each relay answered when we published it
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deliveries: Vec<Delivery>,
}

/// a relay and its answer to something we published, none while it is still on its way
pub type Delivery = (String, Option<Result<(), String>>);

/// an ongoing trade with someone. each has its own conversation, see `Conversation::new`
#[derive(Debug, Clone, Default)]
pub struct Trade {
    /// pubkey hex of the other side
    pub peer: String,
    /// the string that makes the conversation of this trade its own
    pub trade_id: String,
    /// whether the ping that started it is in the event database already
    pub ping_saved: bool,
    /// relays of the offer this trade started from, where both sides publish and listen
    pub relays: Vec<String>,
    pub offer_summary: String,
    pub offer_id: Option<String>,
    /// the terms of that offer. the maker learns which offer it is from the taker's messages.
    pub offer: Option<OfferTerms>,
    /// as the taker: what we expected a bitcoin to cost when the maker's price arrived, from the
    /// offer's price source and premium
    pub price_check: Option<f64>,
    /// messages from before this are left out, they belong to an earlier trade with the same peer
    pub started_at: u32,
    pub started_by_me: bool,
    pub messages: Vec<Message>,
}

/// an offer we published, until it expires or we close it
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MyOffer {
    pub event: Event,
    /// relays that accepted it, where the deletion goes when we close it
    pub relays: Vec<String>,
    /// how each relay took it when we published it, empty for offers of older versions
    #[serde(default)]
    pub deliveries: Vec<Delivery>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct State {
    /// relays currently in use. they are a setting, so they are saved in the config directory
    /// (settings.json). state.json used to hold them, which is only read to carry them over.
    #[serde(default, skip_serializing)]
    pub app_relays: Vec<String>,
    /// our nip-65 write relays, where our offers and reviews go too. none until we found our relay
    /// list or published one.
    #[serde(default)]
    pub write_relays: Option<Vec<String>>,
    /// relays of offers we made recently, where we listen for pings
    #[serde(default)]
    pub ping_relays: Vec<String>,
    /// conversation pubkey hex -> ongoing trade. they are not saved: they are put back together
    /// from the event database when the app starts, see `Store::load_trades`.
    #[serde(skip)]
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

/// what the settings screen changes, saved apart from the app's data
#[derive(serde::Serialize, serde::Deserialize)]
struct Settings {
    app_relays: Vec<String>,
}

pub struct Store {
    /// the app's data
    pub files: Arc<dyn Files>,
    /// our key and the settings, which belong in the user's config directory
    pub config: Arc<dyn Files>,
    /// settings.json as last saved, to write it only when it changes
    settings_json: Mutex<String>,
    pub state: State,
    pub db: RedbEventDatabase,
    /// the trades that are over, see `history.rs`
    pub history: History,
}

impl Store {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(dir: &std::path::Path, config_dir: &std::path::Path) -> Self {
        let _ = std::fs::create_dir_all(config_dir);
        move_keys(dir, config_dir);
        let db = RedbEventDatabase::open(dir.join("events.redb")).expect("failed to open database");
        Self::new(
            Arc::new(Dir(dir.to_path_buf())),
            Arc::new(Dir(config_dir.to_path_buf())),
            db,
        )
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn open() -> Self {
        let db = RedbEventDatabase::open_opfs("events.redb")
            .await
            .expect("failed to open database");
        Self::new(Arc::new(LocalStorage), Arc::new(LocalStorage), db)
    }

    pub(crate) fn new(
        files: Arc<dyn Files>,
        config: Arc<dyn Files>,
        db: RedbEventDatabase,
    ) -> Self {
        let mut state = files
            .read("state.json")
            .and_then(|s| match serde_json::from_str::<State>(&s) {
                Ok(state) => Some(state),
                Err(err) => {
                    // starting empty would overwrite it on the next save, taking the trades and
                    // the block list with it, so keep what is there
                    log::error!("unreadable state.json, kept as state.json.bad: {err}");
                    if let Err(err) = files.write("state.json.bad", &s) {
                        log::error!("failed to keep state.json: {err}");
                    }
                    None
                }
            })
            .unwrap_or_else(|| State {
                app_relays: DEFAULT_RELAYS.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            });
        let saved = config
            .read("settings.json")
            .and_then(|s| serde_json::from_str::<Settings>(&s).ok());
        let carried_over = saved.is_none();
        if let Some(saved) = saved {
            state.app_relays = saved.app_relays;
        }
        let store = Self {
            files,
            config,
            settings_json: Mutex::new(String::new()),
            state,
            history: History::new(db.clone(), PubKey([0; 32])),
            db,
        };
        if carried_over {
            store.persist_settings();
        } else {
            *store.settings_json.lock().unwrap() = store.settings_json();
        }
        store
    }

    fn settings_json(&self) -> String {
        serde_json::to_string(&Settings {
            app_relays: self.state.app_relays.clone(),
        })
        .unwrap_or_default()
    }

    fn persist_settings(&self) {
        let json = self.settings_json();
        let mut written = self.settings_json.lock().unwrap();
        if *written == json {
            return;
        }
        match self.config.write("settings.json", &json) {
            Ok(()) => *written = json,
            Err(err) => log::error!("failed to save the settings: {err}"),
        }
    }

    /// save `new` as our key. the old one stays next to it as `key.<pubkey>.old`, just in case
    pub fn replace_key(&self, old: &SecretKey, new: &SecretKey) -> std::io::Result<()> {
        let backup = format!("key.{}.old", old.pubkey().to_hex());
        self.config.write(&backup, &old.to_hex())?;
        self.config.write("key", &new.to_hex())
    }

    pub fn persist(&self) {
        let res = serde_json::to_string(&self.state)
            .map_err(std::io::Error::other)
            .and_then(|json| self.files.write("state.json", &json));
        if let Err(err) = res {
            log::error!("failed to persist state: {err}");
        }
        self.persist_settings();
    }

    pub fn save_event(&self, evt: &Event) {
        match self.db.save_event(evt) {
            Ok(()) | Err(ritualistic::database::DatabaseError::DuplicateEvent) => {}
            Err(err) => log::warn!("failed to save event {}: {err}", evt.id),
        }
    }

    /// forget an event we saved
    pub fn forget_event(&self, evt: &ritualistic::ID) {
        if let Err(err) = self.db.delete_event(evt.short()) {
            log::warn!("failed to forget event {evt}: {err}");
        }
    }

    pub(crate) fn query(&self, filter: Filter) -> Vec<Event> {
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

    /// trade reviews (8020/8022) by `authors` about `subject`
    pub fn reviews_about(&self, authors: &[PubKey], subject: &PubKey) -> Vec<Event> {
        self.query(Filter {
            kinds: Some(vec![KIND_TRADE_FAILURE, KIND_TRADE_SUCCESS]),
            authors: Some(authors.to_vec()),
            tags: Some(vec![TagQuery("p".into(), vec![subject.to_hex()])]),
            ..Default::default()
        })
    }

    /// all trade reviews (8020/8022) written by `author`
    pub fn reviews_by(&self, author: &PubKey) -> Vec<Event> {
        self.query(Filter {
            kinds: Some(vec![KIND_TRADE_FAILURE, KIND_TRADE_SUCCESS]),
            authors: Some(vec![*author]),
            ..Default::default()
        })
    }

    /// people whose offers we look for on their own relays too: the ones we labeled and never
    /// reviewed a failed trade with, and the ones our last review about was a success from the last
    /// six months. never ourselves, someone we blocked or one of `ignore`, the bots of the practice
    /// market, whose reviews are kept like any others.
    pub fn trusted_people(&self, me: &PubKey, now: u32, ignore: &[PubKey]) -> Vec<PubKey> {
        // subject -> our last review about them
        let mut last: HashMap<PubKey, &Event> = HashMap::new();
        let mut failed: HashSet<PubKey> = HashSet::new();
        let mine = self.reviews_by(me);
        for evt in &mine {
            let Some(pk) = evt.tags.find("p").and_then(|t| t[1].parse::<PubKey>().ok()) else {
                continue;
            };
            if evt.kind == KIND_TRADE_FAILURE {
                failed.insert(pk);
            }
            if last.get(&pk).is_none_or(|l| l.created_at < evt.created_at) {
                last.insert(pk, evt);
            }
        }

        let mut people: Vec<PubKey> = self
            .state
            .labels
            .keys()
            .filter_map(|hex| hex.parse().ok())
            .filter(|pk| !failed.contains(pk))
            .collect();
        for (pk, evt) in last {
            if evt.kind == KIND_TRADE_SUCCESS
                && evt.created_at.0 + TRUST_SUCCESS_SECS >= now
                && !people.contains(&pk)
            {
                people.push(pk);
            }
        }
        people.retain(|pk| {
            pk != me && !ignore.contains(pk) && !self.state.blocked.contains(&pk.to_hex())
        });
        people
    }

    /// the people we have reviewed the most successful trades with, best first, not counting
    /// `ignore`
    pub fn top_trade_partners(&self, me: &PubKey, n: usize, ignore: &[PubKey]) -> Vec<PubKey> {
        let mine = self.query(Filter {
            kinds: Some(vec![KIND_TRADE_SUCCESS]),
            authors: Some(vec![*me]),
            ..Default::default()
        });
        let mut counts: HashMap<PubKey, usize> = HashMap::new();
        for evt in &mine {
            if let Some(pk) = evt.tags.find("p").and_then(|t| t[1].parse::<PubKey>().ok())
                && pk != *me
                && !ignore.contains(&pk)
            {
                *counts.entry(pk).or_default() += 1;
            }
        }
        let mut ranked: Vec<_> = counts.into_iter().collect();
        ranked.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        ranked.into_iter().take(n).map(|(pk, _)| pk).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritualistic::{EventTemplate, Kind, Tags, Timestamp};

    struct NoFiles;

    impl Files for NoFiles {
        fn read(&self, _: &str) -> Option<String> {
            None
        }

        fn write(&self, _: &str, _: &str) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn memory() -> redb::Database {
        redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap()
    }

    #[test]
    fn trusted_people() {
        let now = 1_000_000_000;
        let day = 24 * 60 * 60;
        let me = SecretKey::generate();
        let mut store = Store::new(
            Arc::new(NoFiles),
            Arc::new(NoFiles),
            RedbEventDatabase::from_database(memory()).unwrap(),
        );
        let review = |about: &PubKey, kind: Kind, days_ago: u32| {
            store.save_event(
                &EventTemplate {
                    created_at: Timestamp(now - days_ago * day),
                    kind,
                    tags: Tags(vec![vec!["p".into(), about.to_hex()]]),
                    content: String::new(),
                }
                .finalize(&me),
            )
        };
        let pk = || SecretKey::generate().pubkey();
        let (labeled, labeled_failed, recent, old, recovered, relapsed, blocked) =
            (pk(), pk(), pk(), pk(), pk(), pk(), pk());

        review(&labeled_failed, KIND_TRADE_FAILURE, 400);
        review(&recent, KIND_TRADE_SUCCESS, 10);
        review(&old, KIND_TRADE_SUCCESS, 200);
        review(&recovered, KIND_TRADE_FAILURE, 30);
        review(&recovered, KIND_TRADE_SUCCESS, 20);
        review(&relapsed, KIND_TRADE_SUCCESS, 30);
        review(&relapsed, KIND_TRADE_FAILURE, 20);
        review(&blocked, KIND_TRADE_SUCCESS, 10);
        for pk in [&labeled, &labeled_failed, &blocked, &me.pubkey()] {
            store.state.labels.insert(pk.to_hex(), "x".into());
        }
        store.state.blocked.insert(blocked.to_hex());

        let mut got = store.trusted_people(&me.pubkey(), now, &[]);
        let mut want = vec![labeled, recent, recovered];
        got.sort_by_key(|pk| pk.to_hex());
        want.sort_by_key(|pk| pk.to_hex());
        assert_eq!(got, want);
    }
}

//! trades are not kept anywhere but in the event database: the ping that started one, the rumors
//! of its conversation, our notes about it (`protocol::trade_mark`) and our reviews all refer to
//! its id. at startup the last trades are put back together from those, the ones that are over
//! as the history and the others as the ongoing ones, which live in memory from then on.

use std::collections::{BTreeMap, HashSet};

use ritualistic::database::EventDatabase;
use ritualistic::{Event, Filter, PubKey, RedbEventDatabase, SecretKey, TagQuery};

use crate::deal::{self, OfferTerms};
use crate::protocol::{
    Conversation, KIND_MARK, KIND_PING, KIND_RUMOR, KIND_TRADE_FAILURE, KIND_TRADE_SUCCESS,
    MARK_ARCHIVED, MARK_CONCLUDED, MARK_OFFER, MARK_PRICE_CHECK, TRADE_REF, mark_of, ping_trade_id,
    trade_mark,
};
use crate::store::{Message, State, Store, Trade};

/// how many trades, the most recent ones, are brought back when the app starts
const MAX_TRADES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// concluded, and waiting for the user to review it or archive it
    Concluded,
    Success,
    Failure,
    /// put away without a review
    Archived,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Concluded => "concluded",
            Outcome::Success => "success",
            Outcome::Failure => "failure",
            Outcome::Archived => "archived",
        }
    }
}

/// what the mark of an offer carries, as its content: the offer is long gone from the relays by
/// the time we want to look at the trade again
#[derive(serde::Serialize, serde::Deserialize)]
pub struct OfferMark {
    pub summary: String,
    pub terms: OfferTerms,
    pub relays: Vec<String>,
}

/// a review we published about a past trade
#[derive(Debug, Clone)]
pub struct PublishedReview {
    /// event id hex
    pub id: String,
    pub success: bool,
    pub comment: String,
    pub created_at: u32,
}

#[derive(Debug, Clone)]
pub struct PastTrade {
    /// what ties the events of the trade together, and tells the trades apart
    pub trade_id: String,
    /// pubkey hex
    pub peer: String,
    pub offer_id: Option<String>,
    pub offer_summary: String,
    pub offer: Option<OfferTerms>,
    pub started_at: u32,
    pub started_by_me: bool,
    pub ended_at: u32,
    pub outcome: Outcome,
    /// the comment of the review we published, if any
    pub comment: String,
    /// every review we published about it, oldest first. we can change our mind and review again.
    pub reviews: Vec<PublishedReview>,
    pub messages: Vec<Message>,
}

/// the trades that are over, most recently ended first
pub struct History {
    db: RedbEventDatabase,
    me: PubKey,
    past: Vec<PastTrade>,
}

impl History {
    pub fn new(db: RedbEventDatabase, me: PubKey) -> Self {
        Self {
            db,
            me,
            past: vec![],
        }
    }

    fn insert(&mut self, trade: PastTrade) {
        let at = self
            .past
            .iter()
            .position(|t| t.ended_at < trade.ended_at)
            .unwrap_or(self.past.len());
        self.past.insert(at, trade);
    }

    fn save(&self, evt: &Event) {
        match self.db.save_event(evt) {
            Ok(()) | Err(ritualistic::database::DatabaseError::DuplicateEvent) => {}
            Err(err) => log::warn!("failed to save event {}: {err}", evt.id),
        }
    }

    fn save_mark(&self, peer: &str, trade_id: &str, name: &str) {
        if let Ok(peer) = peer.parse::<PubKey>() {
            self.save(&trade_mark(&self.me, &peer, trade_id, name, vec![], ""));
        }
    }

    /// a trade that just ended: concluded to be reviewed later, reviewed (the review is added
    /// with `add_review`) or archived
    pub fn add(
        &mut self,
        trade: &Trade,
        outcome: Outcome,
        comment: &str,
        ended_at: u32,
    ) -> Result<String, String> {
        let mark = if outcome == Outcome::Archived {
            MARK_ARCHIVED
        } else {
            MARK_CONCLUDED
        };
        self.save_mark(&trade.peer, &trade.trade_id, mark);
        self.insert(PastTrade {
            trade_id: trade.trade_id.clone(),
            peer: trade.peer.clone(),
            offer_id: trade.offer_id.clone(),
            offer_summary: trade.offer_summary.clone(),
            offer: trade.offer.clone(),
            started_at: trade.started_at,
            started_by_me: trade.started_by_me,
            ended_at,
            outcome,
            comment: comment.to_string(),
            reviews: vec![],
            messages: trade.messages.clone(),
        });
        Ok(trade.trade_id.clone())
    }

    /// remember a review we published about a trade. the trade counts as concluded the way the
    /// review says, until it is deleted.
    pub fn add_review(&mut self, trade_id: &str, review: PublishedReview) {
        if let Some(t) = self.past.iter_mut().find(|t| t.trade_id == trade_id) {
            t.outcome = if review.success {
                Outcome::Success
            } else {
                Outcome::Failure
            };
            t.comment = review.comment.clone();
            t.reviews.push(review);
        }
    }

    /// forget one review of a trade and return it. the trade goes by its latest remaining
    /// review, or back to being archived.
    pub fn remove_review(&mut self, trade_id: &str, review_id: &str) -> Option<PublishedReview> {
        let t = self.past.iter_mut().find(|t| t.trade_id == trade_id)?;
        let removed = t
            .reviews
            .iter()
            .position(|r| r.id == review_id)
            .map(|i| t.reviews.remove(i));
        match t.reviews.last() {
            Some(last) => {
                t.outcome = if last.success {
                    Outcome::Success
                } else {
                    Outcome::Failure
                };
                t.comment = last.comment.clone();
            }
            None => {
                t.outcome = Outcome::Archived;
                t.comment.clear();
                let (peer, trade_id) = (t.peer.clone(), t.trade_id.clone());
                self.save_mark(&peer, &trade_id, MARK_ARCHIVED);
            }
        }
        removed
    }

    /// put a trade away without a review
    pub fn archive(&mut self, trade_id: &str) {
        let Some(t) = self.past.iter_mut().find(|t| t.trade_id == trade_id) else {
            return;
        };
        t.outcome = Outcome::Archived;
        let (peer, trade_id) = (t.peer.clone(), t.trade_id.clone());
        self.save_mark(&peer, &trade_id, MARK_ARCHIVED);
    }

    /// most recently ended first
    pub fn list(&self, limit: usize) -> Vec<PastTrade> {
        self.past.iter().take(limit).cloned().collect()
    }

    /// whether any past trade is like `f` says
    pub fn any(&self, f: impl Fn(&PastTrade) -> bool) -> bool {
        self.past.iter().any(f)
    }

    /// the most recently ended trade that is like `f` says
    pub fn latest(&self, f: impl Fn(&PastTrade) -> bool) -> Option<PastTrade> {
        self.past.iter().find(|t| f(t)).cloned()
    }

    pub fn get(&self, trade_id: &str) -> Option<PastTrade> {
        self.past.iter().find(|t| t.trade_id == trade_id).cloned()
    }

    /// forget a trade: its ping, its rumors and our notes. the reviews we published stay, they
    /// are about the person.
    pub fn delete(&mut self, trade_id: &str) {
        let Some(i) = self.past.iter().position(|t| t.trade_id == trade_id) else {
            return;
        };
        let trade = self.past.remove(i);
        let query = |filter: Filter| -> Vec<Event> {
            self.db
                .query_events(&mut [filter])
                .map(|r| {
                    r.filter_map(|r| rkyv::deserialize::<Event, rkyv::rancor::Error>(&*r).ok())
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut doomed = query(Filter {
            tags: Some(vec![TagQuery(TRADE_REF.into(), vec![trade.trade_id.clone()])]),
            ..Default::default()
        });
        doomed.retain(|e| e.kind != KIND_TRADE_SUCCESS && e.kind != KIND_TRADE_FAILURE);
        doomed.extend(
            query(Filter {
                kinds: Some(vec![KIND_PING]),
                ..Default::default()
            })
            .into_iter()
            .filter(|e| ping_trade_id(e) == trade.trade_id),
        );
        for evt in doomed {
            if let Err(err) = self.db.delete_event(evt.id.short()) {
                log::warn!("failed to forget event {}: {err}", evt.id);
            }
        }
    }
}

/// where a trade stands, by the last thing that happened to it
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Standing {
    Concluded,
    Archived,
    Reviewed(bool),
}

impl Store {
    /// put the last trades back together from the event database: the ones that are over go to
    /// the history, the others are the ongoing ones. `relays_for` says where a trade with
    /// no offer behind it talks.
    pub fn load_trades(
        &mut self,
        sk: &SecretKey,
        relays_for: impl Fn(&State, &Trade) -> Vec<String>,
    ) {
        let me = sk.pubkey();
        self.history = History::new(self.db.clone(), me);
        self.state.trades.clear();

        let pings = self.query(Filter {
            kinds: Some(vec![KIND_PING]),
            limit: Some(MAX_TRADES),
            ..Default::default()
        });
        let mut seen = HashSet::new();
        let mut ongoing = BTreeMap::new();
        let mut over = vec![];
        for ping in pings {
            let trade_id = ping_trade_id(&ping);
            if trade_id.is_empty() || !seen.insert(trade_id.clone()) {
                continue;
            }
            let started_by_me = ping.pubkey == me;
            let peer = if started_by_me {
                ping.tags.find("p").and_then(|t| t[1].parse::<PubKey>().ok())
            } else {
                Some(ping.pubkey)
            };
            let Some(peer) = peer else { continue };

            let mut events = self.query(Filter {
                tags: Some(vec![TagQuery(TRADE_REF.into(), vec![trade_id.clone()])]),
                ..Default::default()
            });
            events.sort_by_key(|e| e.created_at);

            let mut trade = Trade {
                peer: peer.to_hex(),
                trade_id: trade_id.clone(),
                started_at: ping.created_at.0,
                started_by_me,
                ping_saved: true,
                ..Default::default()
            };
            let mut offer = None;
            let mut standing: Vec<(u32, Standing)> = vec![];
            let mut reviews = vec![];
            for evt in &events {
                let at = evt.created_at.0;
                if evt.kind == KIND_RUMOR {
                    trade.messages.push(Message {
                        id: evt.id.to_hex(),
                        created_at: at,
                        mine: evt.pubkey == me,
                        text: evt.content.clone(),
                        action: evt.tags.find(deal::TAG).cloned(),
                        event: evt.to_string(),
                        deliveries: vec![],
                    });
                } else if evt.kind == KIND_MARK {
                    match mark_of(evt) {
                        Some(MARK_OFFER) => {
                            offer = serde_json::from_str::<OfferMark>(&evt.content).ok().map(|m| {
                                (m, evt.tags.find("e").map(|t| t[1].clone()))
                            });
                        }
                        Some(MARK_PRICE_CHECK) => trade.price_check = evt.content.parse().ok(),
                        Some(MARK_CONCLUDED) => standing.push((at, Standing::Concluded)),
                        Some(MARK_ARCHIVED) => standing.push((at, Standing::Archived)),
                        _ => {}
                    }
                } else if (evt.kind == KIND_TRADE_SUCCESS || evt.kind == KIND_TRADE_FAILURE)
                    && evt.pubkey == me
                {
                    let success = evt.kind == KIND_TRADE_SUCCESS;
                    standing.push((at, Standing::Reviewed(success)));
                    reviews.push(PublishedReview {
                        id: evt.id.to_hex(),
                        success,
                        comment: evt.content.clone(),
                        created_at: at,
                    });
                }
            }
            match offer {
                Some((mark, offer_id)) => {
                    trade.offer_summary = mark.summary;
                    trade.offer = Some(mark.terms);
                    trade.offer_id = offer_id;
                    trade.relays = mark.relays;
                }
                None => {
                    trade.offer_summary = if started_by_me {
                        "· direct chat".into()
                    } else {
                        "pinged you about your offer".into()
                    };
                }
            }
            if trade.relays.is_empty() {
                trade.relays = relays_for(&self.state, &trade);
            }

            // the last thing that happened to it, and of those that happened together, the one
            // that came after the others
            let Some(&(_, last)) = standing.iter().max_by_key(|(at, s)| (*at, *s)) else {
                ongoing.insert(Conversation::new(sk, peer, &trade_id).pubkey().to_hex(), trade);
                continue;
            };
            let ended_at = standing
                .iter()
                .filter(|(_, s)| matches!(s, Standing::Concluded | Standing::Archived))
                .map(|(at, _)| *at)
                .min()
                .unwrap_or(standing[0].0);
            let (outcome, comment) = match last {
                Standing::Concluded => (Outcome::Concluded, String::new()),
                Standing::Archived => (Outcome::Archived, String::new()),
                Standing::Reviewed(success) => (
                    if success {
                        Outcome::Success
                    } else {
                        Outcome::Failure
                    },
                    reviews.last().map(|r| r.comment.clone()).unwrap_or_default(),
                ),
            };
            over.push(PastTrade {
                trade_id,
                peer: trade.peer,
                offer_id: trade.offer_id,
                offer_summary: trade.offer_summary,
                offer: trade.offer,
                started_at: trade.started_at,
                started_by_me,
                ended_at,
                outcome,
                comment,
                reviews,
                messages: trade.messages,
            });
        }
        // oldest first, so that inserting keeps them in order of ending
        over.sort_by_key(|t| t.ended_at);
        for t in over {
            self.history.insert(t);
        }
        self.state.trades = ongoing;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ritualistic::Timestamp;

    use super::*;
    use crate::protocol::{Rumor, ping, trade_mark_at, trade_review};
    use crate::store::Files;

    struct NoFiles;

    impl Files for NoFiles {
        fn read(&self, _: &str) -> Option<String> {
            None
        }

        fn write(&self, _: &str, _: &str) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn store() -> Store {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .unwrap();
        Store::new(
            Arc::new(NoFiles),
            Arc::new(NoFiles),
            RedbEventDatabase::from_database(db).unwrap(),
        )
    }

    /// a trade the way the app saves it: the ping and the messages, first from us, then from them
    fn start(store: &Store, me: &SecretKey, peer: &SecretKey, id: &str, texts: &[&str]) {
        store.save_event(&ping(me, &peer.pubkey(), id));
        let conv = Conversation::new(me, peer.pubkey(), id);
        for (i, text) in texts.iter().enumerate() {
            let (author, to) = if i % 2 == 0 {
                (me.pubkey(), peer.pubkey())
            } else {
                (peer.pubkey(), me.pubkey())
            };
            let rumor = Rumor {
                id: String::new(),
                pubkey: author.to_hex(),
                created_at: 1_000 + i as u32,
                kind: KIND_RUMOR.0,
                tags: vec![vec!["p".into(), to.to_hex()]],
                content: text.to_string(),
            };
            store.save_event(&conv.local_event(&rumor, &author));
        }
    }

    fn mark(store: &Store, me: &SecretKey, peer: &SecretKey, id: &str, name: &str, at: u32) {
        store.save_event(&trade_mark_at(
            &me.pubkey(),
            &peer.pubkey(),
            id,
            name,
            vec![],
            "",
            Timestamp(at),
        ));
    }

    #[test]
    fn trades_come_back_from_the_events() {
        let me = SecretKey::generate();
        let (a, b, c, d) = (
            SecretKey::generate(),
            SecretKey::generate(),
            SecretKey::generate(),
            SecretKey::generate(),
        );
        let mut store = store();
        start(&store, &me, &a, "going", &["hi", "hello", "how much?"]);
        start(&store, &me, &b, "waiting", &["hi"]);
        start(&store, &me, &c, "dropped", &[]);
        start(&store, &me, &d, "done", &["hey"]);
        mark(&store, &me, &b, "waiting", MARK_CONCLUDED, 2_000);
        mark(&store, &me, &c, "dropped", MARK_ARCHIVED, 3_000);
        mark(&store, &me, &d, "done", MARK_CONCLUDED, 4_000);
        // reviewed in the same second as it ended, and by them too, which doesn't count
        store.save_event(&trade_review(&me, &d.pubkey(), Some("done"), true, "fast"));
        store.save_event(&trade_review(&d, &me.pubkey(), Some("done"), false, "slow"));
        // about nobody's trade
        store.save_event(&trade_review(&me, &a.pubkey(), None, true, "floating"));

        store.load_trades(&me, |_, _| vec!["wss://relay.example/".into()]);

        assert_eq!(store.state.trades.len(), 1);
        let trade = store.state.trades.values().next().unwrap();
        assert_eq!(trade.peer, a.pubkey().to_hex());
        assert_eq!(trade.trade_id, "going");
        assert!(trade.started_by_me && trade.ping_saved);
        assert_eq!(trade.relays, vec!["wss://relay.example/"]);
        let texts: Vec<_> = trade.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["hi", "hello", "how much?"]);
        assert_eq!(
            trade.messages.iter().map(|m| m.mine).collect::<Vec<_>>(),
            [true, false, true]
        );
        // it is the conversation key that finds it again
        let key = Conversation::new(&me, a.pubkey(), "going").pubkey().to_hex();
        assert!(store.state.trades.contains_key(&key));

        let past = store.history.list(10);
        let by = |peer: &SecretKey| {
            past.iter()
                .find(|t| t.peer == peer.pubkey().to_hex())
                .unwrap()
        };
        assert_eq!(past.len(), 3);
        // the most recently ended first
        assert_eq!(past[0].peer, d.pubkey().to_hex());
        assert_eq!(by(&b).outcome, Outcome::Concluded);
        assert_eq!(by(&c).outcome, Outcome::Archived);
        assert_eq!(by(&d).outcome, Outcome::Success);
        assert_eq!(by(&d).comment, "fast");
        assert_eq!(by(&d).reviews.len(), 1);
        assert_eq!(by(&d).ended_at, 4_000);
        assert_eq!(by(&d).messages[0].text, "hey");

        // taking the review back leaves it archived, and that is how it comes back
        let id = by(&d).trade_id.clone();
        let review = by(&d).reviews[0].id.clone();
        assert!(store.history.remove_review(&id, &review).is_some());
        assert_eq!(store.history.get(&id).unwrap().outcome, Outcome::Archived);
        store.forget_event(&review.parse().unwrap());
        store.load_trades(&me, |_, _| vec![]);
        assert_eq!(
            store
                .history
                .latest(|t| t.trade_id == "done")
                .unwrap()
                .outcome,
            Outcome::Archived
        );

        // forgetting a trade takes its ping and messages along, and nothing else
        let id = store.history.latest(|t| t.trade_id == "done").unwrap().trade_id;
        store.history.delete(&id);
        store.load_trades(&me, |_, _| vec![]);
        assert!(store.history.latest(|t| t.trade_id == "done").is_none());
        assert_eq!(store.history.list(10).len(), 2);
        assert_eq!(store.state.trades.len(), 1);
    }

    #[test]
    fn what_we_save_of_a_rumor_is_ours_or_theirs() {
        let me = SecretKey::generate();
        let peer = SecretKey::generate();
        let conv = Conversation::new(&me, peer.pubkey(), "t1");
        let (rumor, _) = conv.wrap(&me.pubkey(), "hi", None, None);
        let ours = conv.local_event(&rumor, &me.pubkey());
        // our own rumor keeps the id it was sent with, and nobody signed it
        assert_eq!(ours.id.to_hex(), rumor.id);
        assert!(!ours.verify_signature());
        assert!(ours.check_id());
        // the same rumor read as theirs is another event
        let theirs = conv.local_event(&rumor, &peer.pubkey());
        assert_ne!(theirs.id, ours.id);
        assert!(theirs.check_id());
        assert_eq!(crate::protocol::trade_of(&theirs), Some("t1"));
    }
}

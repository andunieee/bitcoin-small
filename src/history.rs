//! trades that are over: concluded and waiting for a review, reviewed, or just archived. they live in a redb database,
//! while the ongoing ones live in the json state.

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use crate::deal::OfferTerms;
use crate::store::{Message, Trade};

/// id -> the past trade, as json
const TRADES: TableDefinition<u64, &str> = TableDefinition::new("trades");
/// (ended_at, id), to list the most recent first
const BY_END: TableDefinition<(u32, u64), ()> = TableDefinition::new("trades_by_end");

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

/// a review we published about a past trade, and where, to ask those relays to drop it later
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PublishedReview {
    /// event id hex
    pub id: String,
    pub success: bool,
    pub comment: String,
    pub created_at: u32,
    /// the relays that accepted it
    pub relays: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PastTrade {
    /// the key in the database, not stored in the value
    #[serde(skip)]
    pub id: i64,
    /// pubkey hex
    pub peer: String,
    pub offer_id: Option<String>,
    pub offer_summary: String,
    #[serde(default)]
    pub offer: Option<OfferTerms>,
    pub started_at: u32,
    pub started_by_me: bool,
    pub ended_at: u32,
    pub outcome: Outcome,
    /// the comment of the review we published, if any
    pub comment: String,
    /// every review we published about it, oldest first. we can change our mind and review again.
    #[serde(default, alias = "reports")]
    pub reviews: Vec<PublishedReview>,
    pub messages: Vec<Message>,
}

pub struct History {
    db: Database,
}

impl History {
    /// a file on disk, or one backed by `ritualistic::OpfsBackend` in the browser
    pub fn from_database(db: Database) -> Result<Self, String> {
        // read transactions can't open tables that don't exist yet
        let txn = db.begin_write().map_err(err)?;
        txn.open_table(TRADES).map_err(err)?;
        txn.open_table(BY_END).map_err(err)?;
        txn.commit().map_err(err)?;
        Ok(Self { db })
    }

    pub fn add(
        &mut self,
        peer: &str,
        trade: &Trade,
        outcome: Outcome,
        comment: &str,
        ended_at: u32,
    ) -> Result<i64, String> {
        let past = PastTrade {
            id: 0,
            peer: peer.to_string(),
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
        };
        let json = serde_json::to_string(&past).map_err(|e| e.to_string())?;

        let txn = self.db.begin_write().map_err(err)?;
        let id = {
            let mut trades = txn.open_table(TRADES).map_err(err)?;
            let id = match trades.last().map_err(err)? {
                Some((last, _)) => last.value() + 1,
                None => 1,
            };
            trades.insert(id, json.as_str()).map_err(err)?;
            txn.open_table(BY_END)
                .map_err(err)?
                .insert((ended_at, id), ())
                .map_err(err)?;
            id
        };
        txn.commit().map_err(err)?;
        Ok(id as i64)
    }

    /// remember a review we published about a trade. the trade counts as concluded the way the
    /// review says, until it is deleted.
    pub fn add_review(&self, id: i64, review: PublishedReview) {
        self.update(id, |t| {
            t.outcome = if review.success {
                Outcome::Success
            } else {
                Outcome::Failure
            };
            t.comment = review.comment.clone();
            t.reviews.push(review);
        });
    }

    /// forget one review of a trade and return it. the trade goes by its latest remaining
    /// review, or back to being archived.
    pub fn remove_review(&self, id: i64, review_id: &str) -> Option<PublishedReview> {
        let mut removed = None;
        self.update(id, |t| {
            if let Some(i) = t.reviews.iter().position(|r| r.id == review_id) {
                removed = Some(t.reviews.remove(i));
            }
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
                }
            }
        });
        removed
    }

    /// put a trade away without a review
    pub fn archive(&self, id: i64) {
        self.update(id, |t| t.outcome = Outcome::Archived);
    }

    fn update(&self, id: i64, f: impl FnOnce(&mut PastTrade)) {
        let res = (|| {
            let txn = self.db.begin_write().map_err(err)?;
            {
                let mut trades = txn.open_table(TRADES).map_err(err)?;
                let json = trades
                    .get(id as u64)
                    .map_err(err)?
                    .map(|j| j.value().to_string());
                if let Some(mut t) = json.and_then(|j| past_trade(id as u64, &j)) {
                    f(&mut t);
                    let json = serde_json::to_string(&t).map_err(|e| e.to_string())?;
                    trades.insert(id as u64, json.as_str()).map_err(err)?;
                }
            }
            txn.commit().map_err(err)
        })();
        if let Err(e) = res {
            log::error!("failed to update past trade {id}: {e}");
        }
    }

    /// most recently ended first
    pub fn list(&self, limit: usize) -> Vec<PastTrade> {
        let res = (|| {
            let txn = self.db.begin_read().map_err(err)?;
            let trades = txn.open_table(TRADES).map_err(err)?;
            let by_end = txn.open_table(BY_END).map_err(err)?;
            let mut list = vec![];
            for entry in by_end.iter().map_err(err)?.rev().take(limit) {
                let (_, id) = entry.map_err(err)?.0.value();
                if let Some(json) = trades.get(id).map_err(err)? {
                    list.extend(past_trade(id, json.value()));
                }
            }
            Ok::<_, String>(list)
        })();
        res.unwrap_or_else(|e| {
            log::error!("failed to read past trades: {e}");
            vec![]
        })
    }

    /// whether any past trade is like `f` says, stopping at the first one
    pub fn any(&self, f: impl Fn(&PastTrade) -> bool) -> bool {
        let Ok(txn) = self.db.begin_read() else {
            return false;
        };
        let Ok(trades) = txn.open_table(TRADES) else {
            return false;
        };
        let Ok(iter) = trades.iter() else {
            return false;
        };
        iter.flatten()
            .any(|(id, json)| past_trade(id.value(), json.value()).is_some_and(|t| f(&t)))
    }

    /// the most recently ended trade that is like `f` says
    pub fn latest(&self, f: impl Fn(&PastTrade) -> bool) -> Option<PastTrade> {
        let txn = self.db.begin_read().ok()?;
        let trades = txn.open_table(TRADES).ok()?;
        let by_end = txn.open_table(BY_END).ok()?;
        for entry in by_end.iter().ok()?.rev().flatten() {
            let (_, id) = entry.0.value();
            if let Some(t) = trades
                .get(id)
                .ok()
                .flatten()
                .and_then(|json| past_trade(id, json.value()))
                && f(&t)
            {
                return Some(t);
            }
        }
        None
    }

    pub fn get(&self, id: i64) -> Option<PastTrade> {
        let txn = self.db.begin_read().ok()?;
        let json = txn.open_table(TRADES).ok()?.get(id as u64).ok()??;
        past_trade(id as u64, json.value())
    }

    pub fn delete(&self, id: i64) {
        let res = (|| {
            let txn = self.db.begin_write().map_err(err)?;
            {
                let mut trades = txn.open_table(TRADES).map_err(err)?;
                let removed = trades.remove(id as u64).map_err(err)?;
                if let Some(t) = removed.and_then(|json| past_trade(id as u64, json.value())) {
                    txn.open_table(BY_END)
                        .map_err(err)?
                        .remove((t.ended_at, id as u64))
                        .map_err(err)?;
                }
            }
            txn.commit().map_err(err)
        })();
        if let Err(e) = res {
            log::error!("failed to delete past trade {id}: {e}");
        }
    }
}

fn past_trade(id: u64, json: &str) -> Option<PastTrade> {
    let mut t: PastTrade = serde_json::from_str(json)
        .inspect_err(|e| log::warn!("unreadable past trade {id}: {e}"))
        .ok()?;
    t.id = id as i64;
    Some(t)
}

fn err(e: impl Into<redb::Error>) -> String {
    e.into().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut h = History::from_database(
            Database::builder()
                .create_with_backend(redb::backends::InMemoryBackend::new())
                .unwrap(),
        )
        .unwrap();
        let trade = Trade {
            offer_summary: "it's a 'quoted' summary".into(),
            started_at: 10,
            started_by_me: true,
            messages: vec![Message {
                id: "x".into(),
                created_at: 11,
                mine: true,
                text: "hi 'there'".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        h.add("ab", &trade, Outcome::Success, "fast", 20).unwrap();
        h.add("cd", &trade, Outcome::Archived, "", 30).unwrap();
        h.add("ef", &trade, Outcome::Failure, "", 5).unwrap();

        let all = h.list(10);
        assert_eq!(
            all.iter().map(|t| t.peer.as_str()).collect::<Vec<_>>(),
            vec!["cd", "ab", "ef"]
        );
        assert_eq!(all[0].outcome, Outcome::Archived);
        assert_eq!(all[1].offer_summary, trade.offer_summary);
        assert_eq!(all[1].messages[0].text, "hi 'there'");
        assert!(all[1].started_by_me);
        assert_eq!(all[1].offer_id, None);
        assert_eq!(h.list(1).len(), 1);

        h.delete(all[0].id);
        assert!(h.get(all[0].id).is_none());
        assert_eq!(h.get(all[1].id).unwrap().comment, "fast");
        assert_eq!(h.list(10).len(), 2);

        // ids are not reused while there are later ones
        h.add("gh", &trade, Outcome::Success, "", 40).unwrap();
        assert_eq!(h.list(1)[0].id, 4);
    }
}

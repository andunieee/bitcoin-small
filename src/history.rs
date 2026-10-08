//! trades that are over: concluded with a report, or just archived. they live in sqlite, while
//! the ongoing ones live in the json state.

use std::path::{Path, PathBuf};

use minisqlite::{Connection, Value};

use crate::store::{Message, Trade};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure,
    /// put away without a report
    Archived,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::Failure => "failure",
            Outcome::Archived => "archived",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "success" => Outcome::Success,
            "failure" => Outcome::Failure,
            _ => Outcome::Archived,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PastTrade {
    pub id: i64,
    /// pubkey hex
    pub peer: String,
    pub offer_id: Option<String>,
    pub offer_summary: String,
    pub started_at: u32,
    pub started_by_me: bool,
    pub ended_at: u32,
    pub outcome: Outcome,
    /// the comment of the report we published, if any
    pub comment: String,
    pub messages: Vec<Message>,
}

/// minisqlite connections can't move between threads and the app state does, so this opens one
/// for each operation. they are rare: a trade ending, the trades tab being drawn.
pub struct History {
    path: PathBuf,
}

const COLUMNS: &str = "id, peer, offer_id, offer_summary, started_at, started_by_me, ended_at, \
                       outcome, comment, messages";

impl History {
    pub fn open(dir: &Path) -> Result<Self, String> {
        let history = Self {
            path: dir.join("history.sqlite"),
        };
        history
            .db()?
            .execute(
                "CREATE TABLE IF NOT EXISTS trades(
                id INTEGER PRIMARY KEY,
                peer TEXT NOT NULL,
                offer_id TEXT,
                offer_summary TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                started_by_me INTEGER NOT NULL,
                ended_at INTEGER NOT NULL,
                outcome TEXT NOT NULL,
                comment TEXT NOT NULL,
                messages TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS trades_ended_at ON trades(ended_at);",
            )
            .map_err(|e| e.to_string())?;
        Ok(history)
    }

    fn db(&self) -> Result<Connection, String> {
        Connection::open(&self.path).map_err(|e| e.to_string())
    }

    pub fn add(
        &mut self,
        peer: &str,
        trade: &Trade,
        outcome: Outcome,
        comment: &str,
        ended_at: u32,
    ) -> Result<(), String> {
        let messages = serde_json::to_string(&trade.messages).map_err(|e| e.to_string())?;
        self.db()?
            .execute(&format!(
                "INSERT INTO trades(peer, offer_id, offer_summary, started_at, started_by_me,
                    ended_at, outcome, comment, messages)
                 VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {})",
                text(peer),
                trade.offer_id.as_deref().map(text).unwrap_or("NULL".into()),
                text(&trade.offer_summary),
                trade.started_at,
                trade.started_by_me as i64,
                ended_at,
                text(outcome.as_str()),
                text(comment),
                text(&messages),
            ))
            .map_err(|e| e.to_string())
    }

    /// most recently ended first
    pub fn list(&self, limit: usize) -> Vec<PastTrade> {
        self.select(&format!("ORDER BY ended_at DESC, id DESC LIMIT {limit}"))
    }

    pub fn get(&self, id: i64) -> Option<PastTrade> {
        self.select(&format!("WHERE id = {id}")).into_iter().next()
    }

    pub fn delete(&self, id: i64) {
        let res = self.db().and_then(|mut db| {
            db.execute(&format!("DELETE FROM trades WHERE id = {id}"))
                .map_err(|e| e.to_string())
        });
        if let Err(err) = res {
            log::error!("failed to delete past trade {id}: {err}");
        }
    }

    fn select(&self, rest: &str) -> Vec<PastTrade> {
        let res = self.db().and_then(|mut db| {
            db.query(&format!("SELECT {COLUMNS} FROM trades {rest}"))
                .map_err(|e| e.to_string())
        });
        match res {
            Ok(r) => r.rows.iter().filter_map(|row| past_trade(row)).collect(),
            Err(err) => {
                log::error!("failed to read past trades: {err}");
                vec![]
            }
        }
    }
}

fn past_trade(row: &[Value]) -> Option<PastTrade> {
    let int = |i: usize| match row.get(i)? {
        Value::Integer(v) => Some(*v),
        _ => None,
    };
    let txt = |i: usize| match row.get(i)? {
        Value::Text(v) => Some(v.clone()),
        _ => None,
    };
    Some(PastTrade {
        id: int(0)?,
        peer: txt(1)?,
        offer_id: txt(2),
        offer_summary: txt(3)?,
        started_at: int(4)? as u32,
        started_by_me: int(5)? != 0,
        ended_at: int(6)? as u32,
        outcome: Outcome::from_str(&txt(7)?),
        comment: txt(8)?,
        messages: serde_json::from_str(&txt(9)?).unwrap_or_default(),
    })
}

/// a string literal; minisqlite has no bound parameters
fn text(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir =
            std::env::temp_dir().join(format!("bitcoin-small-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut h = History::open(&dir).unwrap();
        let trade = Trade {
            offer_summary: "it's a 'quoted' summary; DROP TABLE trades".into(),
            started_at: 10,
            started_by_me: true,
            messages: vec![Message {
                id: "x".into(),
                created_at: 11,
                mine: true,
                text: "hi 'there'".into(),
            }],
            ..Default::default()
        };
        h.add("ab", &trade, Outcome::Success, "fast", 20).unwrap();
        h.add("cd", &trade, Outcome::Archived, "", 30).unwrap();

        let all = h.list(10);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].peer, "cd");
        assert_eq!(all[0].outcome, Outcome::Archived);
        assert_eq!(all[1].offer_summary, trade.offer_summary);
        assert_eq!(all[1].messages[0].text, "hi 'there'");
        assert!(all[1].started_by_me);
        assert_eq!(all[1].offer_id, None);

        h.delete(all[0].id);
        assert!(h.get(all[0].id).is_none());
        assert_eq!(h.get(all[1].id).unwrap().comment, "fast");
        drop(h);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

//! the relays, as the app sees them: somewhere to subscribe and publish. the real ones are
//! reached through ritualistic's `Network`. the XFK practice market (see `xfk.rs`) lives in a
//! `MemoryRelay` inside the app, and `Router` sends each url to the one it belongs to.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ritualistic::{Event, Filter, Network, PublishOptions, SubscriptionOptions};
use tokio::sync::{mpsc, oneshot};
use tokio_with_wasm::alias as tokio;

use crate::protocol::now;
use crate::store::Files;

#[derive(Debug)]
pub enum Occurrence {
    Event(Event),
    /// every relay has sent what it had stored, what comes after is new
    Eose,
    /// every relay closed the subscription
    Closed,
}

/// a relay, as a normalized url, and how it took something we published
pub type Answer = (String, Result<(), String>);

pub trait Relays: Send + Sync {
    /// the stored events that match on any of `urls`, then `Eose`, then new ones as they come,
    /// until `Closed` or the receiver is dropped
    fn subscribe(&self, urls: &[String], filter: Filter) -> mpsc::UnboundedReceiver<Occurrence>;

    /// one answer from each relay. invalid urls are skipped.
    fn publish(&self, urls: &[String], evt: Event) -> mpsc::UnboundedReceiver<Answer>;

    /// whether we can connect to it
    fn reachable(&self, url: &str) -> oneshot::Receiver<bool>;
}

impl Relays for Network {
    fn subscribe(&self, urls: &[String], filter: Filter) -> mpsc::UnboundedReceiver<Occurrence> {
        let (tx, rx) = mpsc::unbounded_channel();
        let (net, urls) = (self.clone(), urls.to_vec());
        tokio::spawn(async move {
            let mut sub = net
                .subscribe(&urls, filter, SubscriptionOptions::default())
                .await;
            loop {
                let occ = tokio::select! {
                    occ = sub.recv() => occ,
                    // dropping `sub` is what closes it on the relays
                    _ = tx.closed() => return,
                };
                let occ = match occ {
                    Some(ritualistic::Occurrence::Event(evt, _)) => Occurrence::Event(*evt),
                    Some(ritualistic::Occurrence::EOSE) => Occurrence::Eose,
                    Some(ritualistic::Occurrence::Close) | None => Occurrence::Closed,
                };
                let closed = matches!(occ, Occurrence::Closed);
                if tx.send(occ).is_err() || closed {
                    return;
                }
            }
        });
        rx
    }

    fn publish(&self, urls: &[String], evt: Event) -> mpsc::UnboundedReceiver<Answer> {
        let (tx, rx) = mpsc::unbounded_channel();
        let (mut net, urls) = (self.clone(), urls.to_vec());
        tokio::spawn(async move {
            let mut results = net
                .publish_many(&urls, evt, PublishOptions::default())
                .await;
            while let Some(res) = results.recv().await {
                let answer = match res.error {
                    None => Ok(()),
                    Some(err) => Err(err),
                };
                let _ = tx.send((res.relay_url, answer));
            }
        });
        rx
    }

    fn reachable(&self, url: &str) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        let (net, url) = (self.clone(), url.to_string());
        tokio::spawn(async move {
            let _ = tx.send(net.ensure_relay(&url).await.is_ok());
        });
        rx
    }
}

/// a relay that only exists inside the app. it stores events, as a relay does, in a file when it
/// has one: the conversations of the practice market outlive a restart like real ones do.
pub struct MemoryRelay {
    url: String,
    inner: Mutex<MemoryInner>,
    files: Option<Arc<dyn Files>>,
}

struct MemoryInner {
    events: Vec<Event>,
    subs: Vec<(Filter, mpsc::UnboundedSender<Occurrence>)>,
}

impl MemoryRelay {
    pub fn new(url: &str, files: Option<Arc<dyn Files>>) -> Self {
        let url = ritualistic::normalize_url(url)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| url.to_string());
        let mut events: Vec<Event> = files
            .as_ref()
            .and_then(|f| f.read(&file_name(&url)))
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        events.retain(|e| !expired(e));
        Self {
            url,
            inner: Mutex::new(MemoryInner {
                events,
                subs: vec![],
            }),
            files,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// what a relay does with an event it is sent: check it, keep it unless it is ephemeral, act on
    /// deletions, and pass it to whoever is listening
    fn accept(&self, evt: &Event) -> Result<(), String> {
        if !evt.verify_signature() {
            return Err("invalid: bad signature".into());
        }
        if expired(evt) {
            return Err("invalid: event is expired".into());
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.events.iter().any(|e| e.id == evt.id) {
            return Ok(());
        }
        let kind = evt.kind.0;
        let stored = !(20000..30000).contains(&kind);
        if stored {
            if evt.kind == crate::protocol::KIND_DELETION {
                // nip-09: only the author's own events go
                let ids: Vec<String> = evt.tags.find_all("e").map(|t| t[1].clone()).collect();
                inner
                    .events
                    .retain(|e| e.pubkey != evt.pubkey || !ids.contains(&e.id.to_hex()));
            }
            inner.events.push(evt.clone());
        }
        inner.subs.retain(|(filter, tx)| {
            !tx.is_closed() && (!filter.matches(evt) || tx.send(Occurrence::Event(evt.clone())).is_ok())
        });
        if stored {
            self.persist(&mut inner);
        }
        Ok(())
    }

    fn persist(&self, inner: &mut MemoryInner) {
        let Some(files) = &self.files else { return };
        inner.events.retain(|e| !expired(e));
        let res = serde_json::to_string(&inner.events)
            .map_err(std::io::Error::other)
            .and_then(|json| files.write(&file_name(&self.url), &json));
        if let Err(err) = res {
            log::warn!("failed to save the events of {}: {err}", self.url);
        }
    }
}

impl Relays for MemoryRelay {
    fn subscribe(&self, _: &[String], filter: Filter) -> mpsc::UnboundedReceiver<Occurrence> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut inner = self.inner.lock().unwrap();
        let mut stored: Vec<&Event> = inner
            .events
            .iter()
            .filter(|e| filter.matches(e) && !expired(e))
            .collect();
        // newest first, as relays do, so a limit keeps the newest
        stored.sort_by_key(|e| std::cmp::Reverse(e.created_at));
        stored.truncate(filter.limit.unwrap_or(usize::MAX));
        for evt in stored {
            let _ = tx.send(Occurrence::Event(evt.clone()));
        }
        let _ = tx.send(Occurrence::Eose);
        inner.subs.push((filter, tx));
        rx
    }

    fn publish(&self, _: &[String], evt: Event) -> mpsc::UnboundedReceiver<Answer> {
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = tx.send((self.url.clone(), self.accept(&evt)));
        rx
    }

    fn reachable(&self, _: &str) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(true);
        rx
    }
}

/// the real relays and the one inside the app, each url going to the one it belongs to
pub struct Router {
    pub net: Network,
    pub memory: Arc<MemoryRelay>,
}

impl Router {
    /// (the real relays, whether the one in memory is among them)
    fn split(&self, urls: &[String]) -> (Vec<String>, bool) {
        let mut memory = false;
        let mut real = vec![];
        for url in urls {
            let normalized = ritualistic::normalize_url(url).map(|u| u.to_string());
            if normalized.as_deref() == Ok(self.memory.url()) {
                memory = true;
            } else {
                real.push(url.clone());
            }
        }
        (real, memory)
    }
}

impl Relays for Router {
    fn subscribe(&self, urls: &[String], filter: Filter) -> mpsc::UnboundedReceiver<Occurrence> {
        match self.split(urls) {
            (real, false) => Relays::subscribe(&self.net, &real, filter),
            (real, true) if real.is_empty() => self.memory.subscribe(&[], filter),
            (real, true) => merge(vec![
                Relays::subscribe(&self.net, &real, filter.clone()),
                self.memory.subscribe(&[], filter),
            ]),
        }
    }

    fn publish(&self, urls: &[String], evt: Event) -> mpsc::UnboundedReceiver<Answer> {
        let (real, memory) = self.split(urls);
        if !memory {
            return Relays::publish(&self.net, &real, evt);
        }
        let mut answers = self.memory.publish(&[], evt.clone());
        if real.is_empty() {
            return answers;
        }
        let mut more = Relays::publish(&self.net, &real, evt);
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(a) = answers.recv().await {
                let _ = tx.send(a);
            }
            while let Some(a) = more.recv().await {
                let _ = tx.send(a);
            }
        });
        rx
    }

    fn reachable(&self, url: &str) -> oneshot::Receiver<bool> {
        match self.split(&[url.to_string()]) {
            (_, true) => self.memory.reachable(url),
            _ => Relays::reachable(&self.net, url),
        }
    }
}

/// several subscriptions as one: eose once all of them sent it, closed once all of them closed
fn merge(subs: Vec<mpsc::UnboundedReceiver<Occurrence>>) -> mpsc::UnboundedReceiver<Occurrence> {
    let (tx, rx) = mpsc::unbounded_channel();
    let total = subs.len();
    let eosed = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(AtomicUsize::new(0));
    for mut sub in subs {
        let (tx, eosed, closed) = (tx.clone(), eosed.clone(), closed.clone());
        tokio::spawn(async move {
            let mut counted = false;
            let last = |counter: &AtomicUsize| counter.fetch_add(1, Ordering::SeqCst) + 1 == total;
            loop {
                let occ = tokio::select! {
                    occ = sub.recv() => occ,
                    _ = tx.closed() => return,
                };
                match occ {
                    Some(Occurrence::Event(evt)) => {
                        if tx.send(Occurrence::Event(evt)).is_err() {
                            return;
                        }
                    }
                    Some(Occurrence::Eose) => {
                        counted = true;
                        if last(&eosed) {
                            let _ = tx.send(Occurrence::Eose);
                        }
                    }
                    Some(Occurrence::Closed) | None => break,
                }
            }
            if !counted && last(&eosed) {
                let _ = tx.send(Occurrence::Eose);
            }
            if last(&closed) {
                let _ = tx.send(Occurrence::Closed);
            }
        });
    }
    rx
}

fn file_name(url: &str) -> String {
    let host: String = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("relay-{}.json", host.trim_matches('-'))
}

/// nip-40
fn expired(evt: &Event) -> bool {
    evt.tags
        .find("expiration")
        .and_then(|t| t[1].parse::<u32>().ok())
        .is_some_and(|exp| exp <= now().0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritualistic::{EventTemplate, Kind, SecretKey, Tags, TagQuery};

    fn event(sk: &SecretKey, kind: u16, tags: Vec<Vec<String>>) -> Event {
        EventTemplate {
            created_at: now(),
            kind: Kind(kind),
            tags: Tags(tags),
            content: String::new(),
        }
        .finalize(sk)
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<Occurrence>) -> Vec<Occurrence> {
        let mut got = vec![];
        while let Ok(occ) = rx.try_recv() {
            got.push(occ);
        }
        got
    }

    #[test]
    fn memory_relay() {
        let relay = MemoryRelay::new("wss://xfk.invalid", None);
        assert_eq!(relay.url(), "wss://xfk.invalid/");
        let sk = SecretKey::generate();
        let note = event(&sk, 1, vec![]);
        let mut answers = relay.publish(&[], note.clone());
        assert_eq!(
            answers.try_recv().unwrap(),
            ("wss://xfk.invalid/".into(), Ok(()))
        );

        // stored, then eose, then live
        let mut sub = relay.subscribe(
            &[],
            Filter {
                kinds: Some(vec![Kind(1), Kind(20001)]),
                ..Default::default()
            },
        );
        let got = drain(&mut sub);
        assert!(matches!(&got[..], [Occurrence::Event(e), Occurrence::Eose] if e.id == note.id));
        let ephemeral = event(&sk, 20001, vec![]);
        relay.publish(&[], ephemeral.clone());
        assert!(matches!(&drain(&mut sub)[..], [Occurrence::Event(e)] if e.id == ephemeral.id));

        // ephemeral ones aren't kept, deleted ones go, and only by their author
        let other = SecretKey::generate();
        relay.publish(&[], event(&other, 5, vec![vec!["e".into(), note.id.to_hex()]]));
        let all = |relay: &MemoryRelay| {
            drain(&mut relay.subscribe(
                &[],
                Filter {
                    authors: Some(vec![sk.pubkey()]),
                    ..Default::default()
                },
            ))
            .len()
        };
        assert_eq!(all(&relay), 2);
        relay.publish(&[], event(&sk, 5, vec![vec!["e".into(), note.id.to_hex()]]));
        // the deletion itself stays
        assert_eq!(all(&relay), 2);
        let notes = drain(&mut relay.subscribe(
            &[],
            Filter {
                kinds: Some(vec![Kind(1)]),
                ..Default::default()
            },
        ));
        assert!(matches!(&notes[..], [Occurrence::Eose]));

        // tags and bad signatures
        let mut forged = event(&sk, 1, vec![vec!["p".into(), other.pubkey().to_hex()]]);
        let mut sub = relay.subscribe(
            &[],
            Filter {
                tags: Some(vec![TagQuery("p".into(), vec![other.pubkey().to_hex()])]),
                ..Default::default()
            },
        );
        drain(&mut sub);
        forged.content = "changed".into();
        assert!(relay.publish(&[], forged).try_recv().unwrap().1.is_err());
        let tagged = event(&sk, 1, vec![vec!["p".into(), other.pubkey().to_hex()]]);
        relay.publish(&[], tagged.clone());
        assert!(matches!(&drain(&mut sub)[..], [Occurrence::Event(e)] if e.id == tagged.id));
    }
}

//! what we know about the people we meet: the label we gave them, how we got to know them through
//! the reviews of others, and when we last downloaded the reviews they wrote. all of it is kept
//! in profiles.json, a list of `Profile`, and held in memory.

use std::collections::{BTreeMap, HashSet};

use ritualistic::PubKey;

/// how many ways of getting to know someone we keep, the shortest ones
pub const MAX_RELATIONSHIPS: usize = 5;
/// how long the reviews we downloaded from someone are fresh
pub const REFETCH_SECS: u32 = 24 * 60 * 60;

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    /// pubkey hex
    pub pubkey: String,
    /// a name for them that only we see, never published
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// the ways we got to know them. each one is the people in between, from us to them, as pubkey
    /// hex: empty if we trust them directly, `[bob]` if bob, whom we trust directly, reviewed them
    /// positively, `[bob, alice]` if it was alice, whom bob reviewed positively, and so on.
    #[serde(default)]
    pub relationship: Vec<Vec<String>>,
    /// when we last downloaded the reviews they wrote, zero if we never did
    #[serde(default)]
    pub last_fetched: u32,
}

#[derive(Debug, Default)]
pub struct Profiles {
    entries: BTreeMap<String, Profile>,
}

impl Profiles {
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let list: Vec<Profile> = serde_json::from_str(json)?;
        Ok(Self {
            entries: list.into_iter().map(|p| (p.pubkey.clone(), p)).collect(),
        })
    }

    pub fn to_json(&self) -> String {
        let list: Vec<&Profile> = self.entries.values().collect();
        serde_json::to_string(&list).unwrap_or_default()
    }

    pub fn get(&self, hex: &str) -> Option<&Profile> {
        self.entries.get(hex)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// the entry of this person, a new one if we didn't know them
    pub fn entry(&mut self, hex: &str) -> &mut Profile {
        self.entries
            .entry(hex.to_string())
            .or_insert_with(|| Profile {
                pubkey: hex.to_string(),
                ..Default::default()
            })
    }

    /// pubkey hex -> label, for the people we labeled
    pub fn labels(&self) -> BTreeMap<String, String> {
        self.entries
            .values()
            .filter(|p| !p.label.is_empty())
            .map(|p| (p.pubkey.clone(), p.label.clone()))
            .collect()
    }

    pub fn set_label(&mut self, hex: &str, label: &str) {
        if label.is_empty() && !self.entries.contains_key(hex) {
            return;
        }
        self.entry(hex).label = label.to_string();
    }

    /// forget how we got to know everyone, as it was relative to a key that is not ours anymore
    pub fn forget_relationships(&mut self) {
        for p in self.entries.values_mut() {
            p.relationship.clear();
            p.last_fetched = 0;
        }
    }

    /// add a way of getting to know someone. only the `MAX_RELATIONSHIPS` shortest are kept.
    pub fn add_relationship(&mut self, hex: &str, path: Vec<String>) {
        let p = self.entry(hex);
        if !p.relationship.contains(&path) {
            p.relationship.push(path);
            p.relationship.sort_by_key(|r| r.len());
            p.relationship.truncate(MAX_RELATIONSHIPS);
        }
    }

    pub fn remove_relationship(&mut self, hex: &str, path: &[String]) {
        if let Some(p) = self.entries.get_mut(hex) {
            p.relationship.retain(|r| r != path);
        }
    }

    pub fn mark_fetched(&mut self, hex: &str, now: u32) {
        self.entry(hex).last_fetched = now;
    }

    /// the ways of getting to know them that don't go through anyone in `distrusted`, shortest first
    pub fn paths<'a>(
        &'a self,
        hex: &str,
        distrusted: &'a HashSet<PubKey>,
    ) -> impl Iterator<Item = &'a Vec<String>> {
        self.entries
            .get(hex)
            .filter(|p| !is_distrusted(&p.pubkey, distrusted))
            .into_iter()
            .flat_map(|p| p.relationship.iter())
            .filter(|path| !path.iter().any(|h| is_distrusted(h, distrusted)))
    }

    /// everyone we get to know by some way that doesn't go through anyone distrusted
    pub fn network(&self, distrusted: &HashSet<PubKey>) -> Vec<PubKey> {
        self.entries
            .keys()
            .filter(|hex| self.paths(hex, distrusted).next().is_some())
            .filter_map(|hex| hex.parse().ok())
            .collect()
    }

    /// who to download the reviews of next: someone in our network whose reviews we never got or
    /// got a while ago. the ones we never got first, then the closest to us.
    pub fn next_to_fetch(&self, now: u32, distrusted: &HashSet<PubKey>) -> Option<PubKey> {
        self.entries
            .values()
            .filter(|p| p.last_fetched == 0 || p.last_fetched.saturating_add(REFETCH_SECS) <= now)
            .filter_map(|p| {
                let closest = self.paths(&p.pubkey, distrusted).map(|r| r.len()).min()?;
                Some((p.last_fetched, closest, p.pubkey.parse::<PubKey>().ok()?))
            })
            .min_by_key(|(fetched, closest, _)| (*fetched != 0, *closest, *fetched))
            .map(|(_, _, pk)| pk)
    }
}

fn is_distrusted(hex: &str, distrusted: &HashSet<PubKey>) -> bool {
    hex.parse::<PubKey>()
        .is_ok_and(|pk| distrusted.contains(&pk))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritualistic::SecretKey;

    #[test]
    fn keeps_the_shortest_ways() {
        let mut profiles = Profiles::default();
        for n in [6, 3, 1, 4, 0, 2, 5] {
            profiles.add_relationship("x", (0..n).map(|i| i.to_string()).collect());
        }
        profiles.add_relationship("x", vec![]);
        let lens: Vec<usize> = profiles
            .get("x")
            .unwrap()
            .relationship
            .iter()
            .map(Vec::len)
            .collect();
        assert_eq!(lens, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn picks_who_to_fetch() {
        let pk = || SecretKey::generate().pubkey();
        let (near, far, fetched, stale, bad, via_bad, unknown) =
            (pk(), pk(), pk(), pk(), pk(), pk(), pk());
        let mut profiles = Profiles::default();
        profiles.add_relationship(&near.to_hex(), vec![]);
        profiles.add_relationship(&far.to_hex(), vec![near.to_hex(), near.to_hex()]);
        profiles.add_relationship(&fetched.to_hex(), vec![]);
        profiles.mark_fetched(&fetched.to_hex(), 1000);
        profiles.add_relationship(&stale.to_hex(), vec![near.to_hex()]);
        profiles.mark_fetched(&stale.to_hex(), 10);
        profiles.add_relationship(&bad.to_hex(), vec![]);
        profiles.add_relationship(&via_bad.to_hex(), vec![bad.to_hex()]);
        profiles.entry(&unknown.to_hex());
        let distrusted: HashSet<PubKey> = [bad].into();
        let now = 10 + REFETCH_SECS + 10;

        let mut order = vec![];
        while let Some(next) = profiles.next_to_fetch(now, &distrusted) {
            order.push(next);
            profiles.mark_fetched(&next.to_hex(), now);
        }
        // never fetched ones, closest first, then the one that went stale. not the ones nobody
        // trusts or that are only known through someone distrusted
        assert_eq!(order, vec![near, far, stale]);
        assert_eq!(profiles.network(&distrusted).len(), 4);
    }

    #[test]
    fn json_roundtrip() {
        let mut profiles = Profiles::default();
        profiles.set_label("a", "alice");
        profiles.add_relationship("a", vec!["b".into()]);
        let back = Profiles::from_json(&profiles.to_json()).unwrap();
        assert_eq!(back.get("a"), profiles.get("a"));
        assert_eq!(back.labels().get("a").map(String::as_str), Some("alice"));
    }
}

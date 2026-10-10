//! defaults for the new offer form: every field comes from the last offers we made, so nobody
//! has to fill in the same thing over and over. only when we never made one do we fall back to
//! the system locale and hardcoded choices.

use std::collections::HashMap;

use crate::prices;
use crate::protocol::{DEFAULT_BITCOIN_METHODS, payment_methods_for};

/// free relays where people reach us about an offer. offers themselves go to the paid app relays,
/// which is what makes identities cost something, but there is no reason the chat has to.
pub const DEFAULT_NEGOTIATION_RELAYS: &[&str] = &[
    "wss://relay.primal.net",
    "wss://nos.lol",
    "wss://nostr.mom",
    "wss://relay.damus.io",
];

/// how many of our past offers the form learns from
pub const RECENT_OFFERS: usize = 5;

/// what we filled in for an offer we published, kept to fill the next one
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OfferTemplate {
    pub side: String,
    pub currency: String,
    pub price_api: String,
    pub premium: f64,
    /// (min, max), absent in offers from before amounts existed
    #[serde(default)]
    pub amount: Option<(f64, f64)>,
    pub payment_methods: Vec<String>,
    pub bitcoin_methods: Vec<String>,
    #[serde(default)]
    pub relays: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Defaults {
    pub side: String,
    pub currency: String,
    pub premium: f64,
    pub bitcoin_methods: Vec<String>,
    pub relays: Vec<String>,
}

/// `recent` is most recent first
pub fn defaults(recent: &[OfferTemplate], locale_currency: &str) -> Defaults {
    let fallback_relays = || {
        DEFAULT_NEGOTIATION_RELAYS
            .iter()
            .map(|r| r.to_string())
            .collect()
    };
    let Some(last) = recent.first() else {
        return Defaults {
            side: "sell".into(),
            currency: locale_currency.into(),
            premium: 0.0,
            bitcoin_methods: DEFAULT_BITCOIN_METHODS
                .iter()
                .map(|m| m.to_string())
                .collect(),
            relays: fallback_relays(),
        };
    };
    let avg = recent.iter().map(|t| t.premium).sum::<f64>() / recent.len() as f64;
    // when nothing reaches a majority the last offer is the best guess we have
    let or_last = |common: Vec<String>, last: &Vec<String>| {
        if common.is_empty() {
            last.clone()
        } else {
            common
        }
    };
    let mut relays = or_last(majority(recent.iter().map(|t| &t.relays)), &last.relays);
    if relays.is_empty() {
        // only offers from before we remembered relays
        relays = fallback_relays();
    }
    Defaults {
        side: mode(recent.iter().map(|t| t.side.as_str()))
            .unwrap_or("sell")
            .into(),
        currency: mode(recent.iter().map(|t| t.currency.as_str()))
            .unwrap_or(locale_currency)
            .into(),
        premium: (avg * 100.0).round() / 100.0,
        bitcoin_methods: or_last(
            majority(recent.iter().map(|t| &t.bitcoin_methods)),
            &last.bitcoin_methods,
        ),
        relays,
    }
}

/// the api we used most for this currency, else the one we used most overall that can quote it,
/// else the first one that can
pub fn default_api(recent: &[OfferTemplate], currency: &str) -> &'static str {
    let apis = prices::apis_for(currency);
    let usable = |t: &OfferTemplate| apis.iter().any(|a| a.id() == t.price_api);
    let same_currency: Vec<_> = recent.iter().filter(|t| t.currency == currency).collect();
    let pool = if same_currency.iter().any(|t| usable(t)) {
        same_currency
    } else {
        recent.iter().collect()
    };
    mode(
        pool.iter()
            .filter(|t| usable(t))
            .map(|t| t.price_api.as_str()),
    )
    .and_then(|id| apis.iter().find(|a| a.id() == id))
    .or(apis.first())
    .map(|a| a.id())
    .unwrap_or("")
}

/// the amount of our last offer in this currency. amounts don't carry over between currencies
pub fn default_amount(recent: &[OfferTemplate], currency: &str) -> Option<(f64, f64)> {
    recent
        .iter()
        .filter(|t| t.currency == currency)
        .find_map(|t| t.amount)
}

/// payment methods we used in most of our recent offers in this currency. for a currency we never
/// offered in, the ones we use most elsewhere that are also common for it
pub fn default_payment_methods(recent: &[OfferTemplate], currency: &str) -> Vec<String> {
    let same_currency: Vec<_> = recent.iter().filter(|t| t.currency == currency).collect();
    if let Some(last) = same_currency.first() {
        let common = majority(same_currency.iter().map(|t| &t.payment_methods));
        return if common.is_empty() {
            last.payment_methods.clone()
        } else {
            common
        };
    }
    let (fits, _) = payment_methods_for(currency);
    let common = majority(recent.iter().map(|t| &t.payment_methods));
    let last = recent
        .first()
        .map(|t| t.payment_methods.as_slice())
        .unwrap_or_default();
    let pick = |list: &[String]| -> Vec<String> {
        list.iter()
            .filter(|m| fits.contains(&m.as_str()))
            .cloned()
            .collect()
    };
    let picked = pick(&common);
    if picked.is_empty() {
        pick(last)
    } else {
        picked
    }
}

/// the currency of the country in the system locale, USD when it can't tell
pub fn locale_currency() -> String {
    sys_locale::get_locales()
        .find_map(|l| currency_for_locale(&l))
        .unwrap_or("USD")
        .into()
}

/// "pt-BR", "pt_BR.UTF-8" -> "BRL", as long as it is a currency we have prices for
fn currency_for_locale(locale: &str) -> Option<&'static str> {
    let region = locale
        .split(['.', '@'])
        .next()?
        .split(['-', '_'])
        .skip(1)
        .find(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic()))?
        .to_ascii_uppercase();
    let currency = match region.as_str() {
        "US" | "EC" | "SV" | "PA" | "PR" => "USD",
        "AT" | "BE" | "CY" | "DE" | "EE" | "ES" | "FI" | "FR" | "GR" | "HR" | "IE" | "IT"
        | "LT" | "LU" | "LV" | "MT" | "NL" | "PT" | "SI" | "SK" | "AD" | "MC" | "SM" | "VA"
        | "ME" | "XK" => "EUR",
        "GB" => "GBP",
        "CA" => "CAD",
        "AU" => "AUD",
        "CH" | "LI" => "CHF",
        "JP" => "JPY",
        "BR" => "BRL",
        "AR" => "ARS",
        "VE" => "VES",
        "MX" => "MXN",
        "CO" => "COP",
        "CL" => "CLP",
        "PE" => "PEN",
        "CU" => "CUP",
        "NG" => "NGN",
        "KE" => "KES",
        "ZA" => "ZAR",
        "IN" => "INR",
        "CR" => "CRC",
        _ => return None,
    };
    prices::currency_codes()
        .contains(&currency)
        .then_some(currency)
}

/// the most common value; ties go to the most recent
fn mode<'a>(values: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let mut counts: Vec<(&str, usize)> = vec![];
    for v in values.filter(|v| !v.is_empty()) {
        match counts.iter_mut().find(|(x, _)| *x == v) {
            Some((_, n)) => *n += 1,
            None => counts.push((v, 1)),
        }
    }
    // max_by_key returns the last max, so look from the oldest end to land on the most recent
    counts
        .into_iter()
        .rev()
        .max_by_key(|(_, n)| *n)
        .map(|(v, _)| v)
}

/// the items that show up in at least half of the lists, in the order they first appear
fn majority<'a>(lists: impl Iterator<Item = &'a Vec<String>>) -> Vec<String> {
    let lists: Vec<_> = lists.collect();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut order = vec![];
    for list in &lists {
        for item in list.iter() {
            let n = counts.entry(item).or_default();
            if *n == 0 {
                order.push(item.clone());
            }
            *n += 1;
        }
    }
    order
        .into_iter()
        .filter(|item| counts[item.as_str()] * 2 >= lists.len())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tpl(currency: &str, premium: f64, payments: &[&str], bitcoin: &[&str]) -> OfferTemplate {
        tpl_relays(currency, premium, payments, bitcoin, &[])
    }

    fn tpl_relays(
        currency: &str,
        premium: f64,
        payments: &[&str],
        bitcoin: &[&str],
        relays: &[&str],
    ) -> OfferTemplate {
        OfferTemplate {
            side: "sell".into(),
            currency: currency.into(),
            price_api: "yadio".into(),
            premium,
            amount: None,
            payment_methods: payments.iter().map(|s| s.to_string()).collect(),
            bitcoin_methods: bitcoin.iter().map(|s| s.to_string()).collect(),
            relays: relays.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn no_history() {
        let d = defaults(&[], "BRL");
        assert_eq!(d.currency, "BRL");
        assert_eq!(d.side, "sell");
        assert_eq!(d.premium, 0.0);
        assert_eq!(d.bitcoin_methods, vec!["onchain", "lightning", "arkade"]);
        assert_eq!(d.relays, DEFAULT_NEGOTIATION_RELAYS);
        assert_eq!(default_api(&[], "USD"), "coingecko");
        assert_eq!(default_api(&[], "VES"), "yadio");
        assert!(default_payment_methods(&[], "USD").is_empty());
        assert_eq!(default_amount(&[], "USD"), None);
    }

    #[test]
    fn learns_from_recent_offers() {
        let recent = vec![
            tpl("BRL", 2.0, &["PIX"], &["lightning"]),
            tpl("USD", 1.0, &["Zelle"], &["lightning", "onchain"]),
            tpl("BRL", 0.0, &["PIX", "Nubank"], &["lightning", "liquid"]),
        ];
        let d = defaults(&recent, "EUR");
        assert_eq!(d.currency, "BRL");
        assert_eq!(d.premium, 1.0);
        assert_eq!(d.bitcoin_methods, vec!["lightning"]);
        assert_eq!(
            default_payment_methods(&recent, "BRL"),
            vec!["PIX", "Nubank"]
        );
        assert_eq!(default_payment_methods(&recent, "USD"), vec!["Zelle"]);
        assert_eq!(default_api(&recent, "BRL"), "yadio");
    }

    #[test]
    fn amount_from_the_same_currency() {
        let mut usd = tpl("USD", 0.0, &[], &[]);
        usd.amount = Some((20.0, 100.0));
        let mut brl = tpl("BRL", 0.0, &[], &[]);
        brl.amount = Some((50.0, 50.0));
        let recent = vec![tpl("USD", 0.0, &[], &[]), usd, brl];
        assert_eq!(default_amount(&recent, "USD"), Some((20.0, 100.0)));
        assert_eq!(default_amount(&recent, "BRL"), Some((50.0, 50.0)));
        assert_eq!(default_amount(&recent, "EUR"), None);
    }

    #[test]
    fn ties_go_to_the_most_recent() {
        let recent = vec![tpl("USD", 0.0, &[], &[]), tpl("EUR", 0.0, &[], &[])];
        assert_eq!(defaults(&recent, "BRL").currency, "USD");
    }

    #[test]
    fn history_beats_settings() {
        let recent = vec![
            tpl_relays("USD", 0.0, &["Zelle", "Strike"], &["liquid"], &["wss://x/"]),
            tpl_relays(
                "USD",
                0.0,
                &["Zelle"],
                &["onchain"],
                &["wss://x/", "wss://y/"],
            ),
            tpl_relays("USD", 0.0, &["Zelle"], &["arkade"], &["wss://y/"]),
        ];
        let d = defaults(&recent, "BRL");
        assert_eq!(d.relays, vec!["wss://x/", "wss://y/"]);
        // no method reaches a majority: repeat the last offer, not the hardcoded defaults
        assert_eq!(d.bitcoin_methods, vec!["liquid"]);
        // a currency we never offered in still learns from the others
        assert_eq!(default_api(&recent, "EUR"), "yadio");
        assert_eq!(default_payment_methods(&recent, "EUR"), vec!["Strike"]);
        // old history without relays
        let d = defaults(&[tpl("USD", 0.0, &[], &["onchain"])], "BRL");
        assert_eq!(d.relays, DEFAULT_NEGOTIATION_RELAYS);
    }

    #[test]
    fn locales() {
        assert_eq!(currency_for_locale("pt-BR"), Some("BRL"));
        assert_eq!(currency_for_locale("pt_BR.UTF-8"), Some("BRL"));
        assert_eq!(currency_for_locale("de-AT"), Some("EUR"));
        assert_eq!(currency_for_locale("zh-Hans-CN"), None);
        assert_eq!(currency_for_locale("en"), None);
    }
}

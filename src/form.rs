//! defaults for the new offer form: the currency from the system locale, everything else from
//! the last offers we made, so nobody has to fill in the same thing over and over.

use std::collections::HashMap;

use crate::prices;
use crate::protocol::DEFAULT_BITCOIN_METHODS;

/// how many of our past offers the form learns from
pub const RECENT_OFFERS: usize = 5;

/// what we filled in for an offer we published, kept to fill the next one
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OfferTemplate {
    pub side: String,
    pub currency: String,
    pub price_api: String,
    pub premium: f64,
    pub payment_methods: Vec<String>,
    pub bitcoin_methods: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Defaults {
    pub side: String,
    pub currency: String,
    pub premium: f64,
    pub bitcoin_methods: Vec<String>,
}

/// `recent` is most recent first
pub fn defaults(recent: &[OfferTemplate], locale_currency: &str) -> Defaults {
    let premium = if recent.is_empty() {
        0.0
    } else {
        let avg = recent.iter().map(|t| t.premium).sum::<f64>() / recent.len() as f64;
        (avg * 100.0).round() / 100.0
    };
    let mut bitcoin_methods = majority(recent.iter().map(|t| &t.bitcoin_methods));
    if bitcoin_methods.is_empty() {
        bitcoin_methods = DEFAULT_BITCOIN_METHODS
            .iter()
            .map(|m| m.to_string())
            .collect();
    }
    Defaults {
        side: mode(recent.iter().map(|t| t.side.as_str()))
            .unwrap_or("sell")
            .into(),
        currency: mode(recent.iter().map(|t| t.currency.as_str()))
            .unwrap_or(locale_currency)
            .into(),
        premium,
        bitcoin_methods,
    }
}

/// the api we used most for this currency, or the first one that can quote it
pub fn default_api(recent: &[OfferTemplate], currency: &str) -> &'static str {
    let apis = prices::apis_for(currency);
    mode(
        recent
            .iter()
            .filter(|t| t.currency == currency)
            .map(|t| t.price_api.as_str()),
    )
    .and_then(|id| apis.iter().find(|a| a.id() == id))
    .or(apis.first())
    .map(|a| a.id())
    .unwrap_or("")
}

/// payment methods we used in most of our recent offers in this currency
pub fn default_payment_methods(recent: &[OfferTemplate], currency: &str) -> Vec<String> {
    majority(
        recent
            .iter()
            .filter(|t| t.currency == currency)
            .map(|t| &t.payment_methods),
    )
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
        OfferTemplate {
            side: "sell".into(),
            currency: currency.into(),
            price_api: "yadio".into(),
            premium,
            payment_methods: payments.iter().map(|s| s.to_string()).collect(),
            bitcoin_methods: bitcoin.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn no_history() {
        let d = defaults(&[], "BRL");
        assert_eq!(d.currency, "BRL");
        assert_eq!(d.side, "sell");
        assert_eq!(d.premium, 0.0);
        assert_eq!(d.bitcoin_methods, vec!["onchain", "lightning", "arkade"]);
        assert_eq!(default_api(&[], "USD"), "coingecko");
        assert_eq!(default_api(&[], "VES"), "yadio");
        assert!(default_payment_methods(&[], "USD").is_empty());
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
    fn ties_go_to_the_most_recent() {
        let recent = vec![tpl("USD", 0.0, &[], &[]), tpl("EUR", 0.0, &[], &[])];
        assert_eq!(defaults(&recent, "BRL").currency, "USD");
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

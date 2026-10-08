//! hardcoded fiat currencies and the market price apis that can quote bitcoin in each of them.

use std::time::Duration;

/// a market price source; the `id` is what goes in the offer's `price_api` tag
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceApi {
    Coingecko,
    Yadio,
    Kraken,
    Bitstamp,
    Mempool,
}

pub const ALL_APIS: &[PriceApi] = &[
    PriceApi::Coingecko,
    PriceApi::Yadio,
    PriceApi::Kraken,
    PriceApi::Bitstamp,
    PriceApi::Mempool,
];

/// (currency code, price apis that can quote btc in that currency)
pub const CURRENCIES: &[(&str, &[PriceApi])] = {
    use PriceApi::*;
    &[
        ("USD", &[Coingecko, Yadio, Kraken, Bitstamp, Mempool]),
        ("EUR", &[Coingecko, Yadio, Kraken, Bitstamp, Mempool]),
        ("GBP", &[Coingecko, Yadio, Kraken, Bitstamp, Mempool]),
        ("CAD", &[Coingecko, Yadio, Kraken, Mempool]),
        ("AUD", &[Coingecko, Yadio, Kraken, Mempool]),
        ("CHF", &[Coingecko, Yadio, Kraken, Mempool]),
        ("JPY", &[Coingecko, Yadio, Kraken, Mempool]),
        ("BRL", &[Coingecko, Yadio]),
        ("ARS", &[Coingecko, Yadio]),
        ("VES", &[Yadio]),
        ("MXN", &[Coingecko, Yadio]),
        ("COP", &[Yadio]),
        ("CLP", &[Coingecko, Yadio]),
        ("PEN", &[Yadio]),
        ("CUP", &[Yadio]),
        ("NGN", &[Coingecko, Yadio]),
        ("KES", &[Yadio]),
        ("ZAR", &[Coingecko, Yadio]),
        ("INR", &[Coingecko, Yadio]),
        ("CRC", &[Yadio]),
    ]
};

pub fn currency_codes() -> Vec<&'static str> {
    CURRENCIES.iter().map(|(c, _)| *c).collect()
}

pub fn apis_for(currency: &str) -> &'static [PriceApi] {
    CURRENCIES
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(currency))
        .map(|(_, apis)| *apis)
        .unwrap_or(&[])
}

impl PriceApi {
    pub fn id(&self) -> &'static str {
        match self {
            PriceApi::Coingecko => "coingecko",
            PriceApi::Yadio => "yadio",
            PriceApi::Kraken => "kraken",
            PriceApi::Bitstamp => "bitstamp",
            PriceApi::Mempool => "mempool",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        ALL_APIS.iter().copied().find(|api| api.id() == id)
    }

    /// fetch the price of 1 BTC in `currency`
    pub async fn fetch(&self, currency: &str) -> Result<f64, String> {
        let upper = currency.to_uppercase();
        let lower = currency.to_lowercase();
        match self {
            PriceApi::Coingecko => fetch_coingecko(&lower).await,
            PriceApi::Yadio => fetch_yadio(&upper).await,
            PriceApi::Kraken => fetch_kraken(&upper).await,
            PriceApi::Bitstamp => fetch_bitstamp(&lower).await,
            PriceApi::Mempool => fetch_mempool(&upper).await,
        }
    }
}

async fn get_json(url: &str) -> Result<serde_json::Value, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("bitcoin-small")
        .build()
        .map_err(|e| e.to_string())?
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())
}

/// some apis return numbers, others return numeric strings
fn as_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.parse().ok())
}

async fn fetch_coingecko(cur: &str) -> Result<f64, String> {
    let url =
        format!("https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies={cur}");
    let j = get_json(&url).await?;
    as_f64(&j["bitcoin"][cur]).ok_or_else(|| format!("coingecko has no price for {cur}"))
}

async fn fetch_yadio(cur: &str) -> Result<f64, String> {
    let j = get_json("https://api.yadio.io/exrates/BTC").await?;
    as_f64(&j["BTC"][cur]).ok_or_else(|| format!("yadio has no price for {cur}"))
}

async fn fetch_kraken(cur: &str) -> Result<f64, String> {
    let j = get_json(&format!(
        "https://api.kraken.com/0/public/Ticker?pair=XBT{cur}"
    ))
    .await?;
    // the result is keyed by kraken's own pair name (e.g. "XXBTZUSD"), so just take the first one
    j["result"]
        .as_object()
        .and_then(|r| r.values().next())
        .and_then(|pair| as_f64(&pair["c"][0]))
        .ok_or_else(|| format!("kraken has no price for {cur}"))
}

async fn fetch_bitstamp(cur: &str) -> Result<f64, String> {
    let j = get_json(&format!("https://www.bitstamp.net/api/v2/ticker/btc{cur}/")).await?;
    as_f64(&j["last"]).ok_or_else(|| format!("bitstamp has no price for {cur}"))
}

async fn fetch_mempool(cur: &str) -> Result<f64, String> {
    let j = get_json("https://mempool.space/api/v1/prices").await?;
    as_f64(&j[cur])
        .filter(|p| *p > 0.0)
        .ok_or_else(|| format!("mempool has no price for {cur}"))
}

/// parse a premium like "+1%", "-0.5%", "2" into a percentage; anything empty means 0%
pub fn parse_premium(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('%').trim();
    if s.is_empty() {
        return Some(0.0);
    }
    s.trim_start_matches('+')
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

/// canonical form of a premium for the offer tag
pub fn format_premium(pct: f64) -> String {
    format!("{}{}%", if pct >= 0.0 { "+" } else { "" }, pct)
}

pub fn apply_premium(market: f64, pct: f64) -> f64 {
    market * (1.0 + pct / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premium() {
        assert_eq!(parse_premium("+1%"), Some(1.0));
        assert_eq!(parse_premium("-0.5%"), Some(-0.5));
        assert_eq!(parse_premium(""), Some(0.0));
        assert_eq!(parse_premium("x"), None);
        assert_eq!(format_premium(1.0), "+1%");
        assert_eq!(format_premium(-2.5), "-2.5%");
        assert_eq!(apply_premium(100.0, 1.0), 101.0);
    }
}

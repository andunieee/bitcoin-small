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
    /// the made-up price of the XFK practice market, see `xfk.rs`
    Xfk,
}

pub const ALL_APIS: &[PriceApi] = &[
    PriceApi::Coingecko,
    PriceApi::Yadio,
    PriceApi::Kraken,
    PriceApi::Bitstamp,
    PriceApi::Mempool,
    PriceApi::Xfk,
];

/// (currency code, price apis that can quote btc in that currency)
pub const CURRENCIES: &[(&str, &[PriceApi])] = {
    use PriceApi::*;
    &[
        // first, as it is where newcomers start
        ("XFK", &[Xfk]),
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
            PriceApi::Xfk => crate::xfk::PRICE_API,
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
            PriceApi::Xfk if crate::xfk::is_xfk(currency) => Ok(crate::xfk::PRICE),
            PriceApi::Xfk => Err(format!("{} only quotes XFK", crate::xfk::PRICE_API)),
        }
    }
}

async fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let get = async {
        // browsers set the user agent themselves
        #[cfg(not(target_arch = "wasm32"))]
        let client = reqwest::Client::builder()
            .user_agent("bitcoin-smol")
            .build();
        #[cfg(target_arch = "wasm32")]
        let client = reqwest::Client::builder().build();
        client
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
    };
    tokio_with_wasm::alias::time::timeout(Duration::from_secs(15), get)
        .await
        .map_err(|_| "timed out".to_string())?
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

/// the most a premium can be above or below the market price, in percent
pub const MAX_PREMIUM: f64 = 15.0;

/// parse a premium like "+1%", "-0.5%", "2" into a percentage; anything empty means 0%. it has
/// to be within MAX_PREMIUM either way, offers beyond that are ignored.
pub fn parse_premium(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('%').trim();
    if s.is_empty() {
        return Some(0.0);
    }
    s.trim_start_matches('+')
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && v.abs() <= MAX_PREMIUM)
}

/// canonical form of a premium for the offer tag
pub fn format_premium(pct: f64) -> String {
    format!("{}{}%", if pct >= 0.0 { "+" } else { "" }, pct)
}

pub fn apply_premium(market: f64, pct: f64) -> f64 {
    market * (1.0 + pct / 100.0)
}

/// parse a fiat amount like "20" or "150.50"; it has to be more than zero
pub fn parse_amount(s: &str) -> Option<f64> {
    s.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
}

/// canonical form of an amount for the offer tag
pub fn format_amount(v: f64) -> String {
    v.to_string()
}

pub fn thousands(v: f64) -> String {
    let int = v.round() as i64;
    let digits = int.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if int < 0 { format!("-{out}") } else { out }
}

/// a fiat amount with thousands separators, and cents only when there are any
pub fn fiat(v: f64) -> String {
    let cents = (v * 100.0).round() as i64;
    let int = thousands((cents / 100) as f64);
    match cents % 100 {
        0 => int,
        c => format!("{int}.{c:02}"),
    }
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
        assert_eq!(parse_premium("-100%"), None);
        assert_eq!(parse_premium("-15.1"), None);
        assert_eq!(parse_premium("15.1%"), None);
        assert_eq!(parse_premium("-15%"), Some(-15.0));
        assert_eq!(parse_premium("+15"), Some(15.0));
        assert_eq!(format_premium(1.0), "+1%");
        assert_eq!(format_premium(-2.5), "-2.5%");
        assert_eq!(apply_premium(100.0, 1.0), 101.0);
    }

    #[test]
    fn amount() {
        assert_eq!(parse_amount(" 20 "), Some(20.0));
        assert_eq!(parse_amount("150.5"), Some(150.5));
        assert_eq!(parse_amount("0"), None);
        assert_eq!(parse_amount("-5"), None);
        assert_eq!(parse_amount(""), None);
        assert_eq!(format_amount(20.0), "20");
        assert_eq!(format_amount(150.5), "150.5");
    }
}

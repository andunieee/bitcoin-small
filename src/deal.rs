//! the structured part of a trade chat. some messages carry an action in a `["trade", …]` tag of
//! the rumor, next to an english text that says the same for people (and clients) that don't
//! read it. both sides replay those actions, in order, to arrive at the same state of the deal.
//!
//! the taker picks the amount and the methods among what the offer allows, each side tells the
//! other where to send what they receive, then the taker sends their side first, and the maker
//! after that.

use crate::prices::{self, fiat};
use crate::protocol::{Offer, Side};
use crate::store::Message;

/// the name of the rumor tag that carries an action
pub const TAG: &str = "trade";

/// what a trade needs from the offer it started from, kept with the trade
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OfferTerms {
    /// whether we made the offer, as opposed to taking it
    pub mine: bool,
    pub side: Side,
    pub currency: String,
    /// where the market price comes from, and how far from it the trade goes. empty and zero in
    /// trades from before these were kept.
    #[serde(default)]
    pub price_api: String,
    #[serde(default)]
    pub premium: f64,
    pub amount_min: f64,
    pub amount_max: f64,
    pub payment_methods: Vec<String>,
    pub bitcoin_methods: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Maker,
    Taker,
}

impl Role {
    pub fn other(self) -> Self {
        match self {
            Role::Maker => Role::Taker,
            Role::Taker => Role::Maker,
        }
    }
}

impl OfferTerms {
    pub fn from_offer(offer: &Offer, mine: bool) -> Self {
        OfferTerms {
            mine,
            side: offer.side,
            currency: offer.currency.clone(),
            price_api: offer.price_api.clone(),
            premium: offer.premium,
            amount_min: offer.amount_min,
            amount_max: offer.amount_max,
            payment_methods: offer.payment_methods.clone(),
            bitcoin_methods: offer.bitcoin_methods.clone(),
        }
    }

    pub fn my_role(&self) -> Role {
        if self.mine { Role::Maker } else { Role::Taker }
    }

    /// whoever ends up with the bitcoin. the other one ends up with the fiat.
    pub fn bitcoin_receiver(&self) -> Role {
        match self.side {
            Side::Sell => Role::Taker,
            Side::Buy => Role::Maker,
        }
    }

    pub fn fixed_amount(&self) -> bool {
        fiat(self.amount_min) == fiat(self.amount_max)
    }

    /// what `who` sends: "the bitcoin" or "the 50 USD"
    fn what_sends(&self, who: Role, deal: &Deal) -> String {
        if who == self.bitcoin_receiver() {
            match deal.amount {
                Some(a) => format!("the {} {}", fiat(a), self.currency),
                None => format!("the {}", self.currency),
            }
        } else {
            "the bitcoin".into()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// how much fiat, in the offer's currency
    Amount(f64),
    BitcoinMethod(String),
    PaymentMethod(String),
    /// where the bitcoin goes: an address, an invoice…
    BitcoinDetails(String),
    /// where the fiat goes: bank details, a phone number…
    PaymentDetails(String),
    /// the sender has sent their side
    Sent,
}

impl Action {
    pub fn to_tag(&self) -> Vec<String> {
        let (name, value) = match self {
            Action::Amount(a) => ("amount", Some(prices::format_amount(*a))),
            Action::BitcoinMethod(m) => ("bitcoin-method", Some(m.clone())),
            Action::PaymentMethod(m) => ("payment-method", Some(m.clone())),
            Action::BitcoinDetails(d) => ("bitcoin-details", Some(d.clone())),
            Action::PaymentDetails(d) => ("payment-details", Some(d.clone())),
            Action::Sent => ("sent", None),
        };
        let mut tag = vec![TAG.to_string(), name.to_string()];
        tag.extend(value);
        tag
    }

    pub fn from_tag(tag: &[String]) -> Option<Self> {
        if tag.first()? != TAG {
            return None;
        }
        let value = || tag.get(2).cloned();
        Some(match tag.get(1)?.as_str() {
            "amount" => Action::Amount(prices::parse_amount(tag.get(2)?)?),
            "bitcoin-method" => Action::BitcoinMethod(value()?),
            "payment-method" => Action::PaymentMethod(value()?),
            "bitcoin-details" => Action::BitcoinDetails(value()?),
            "payment-details" => Action::PaymentDetails(value()?),
            "sent" => Action::Sent,
            _ => return None,
        })
    }

    /// the text of the message, as said by `who`, before the action is applied to `deal`
    pub fn text(&self, terms: &OfferTerms, who: Role, deal: &Deal) -> String {
        match self {
            Action::Amount(a) => format!("Let's trade {} {}.", fiat(*a), terms.currency),
            Action::BitcoinMethod(m) => format!("Let's send the bitcoin via {m}."),
            Action::PaymentMethod(m) => format!("Let's send the {} via {m}.", terms.currency),
            Action::BitcoinDetails(d) => format!("Send the bitcoin to:\n{d}"),
            Action::PaymentDetails(d) => format!("Send the {} to:\n{d}", terms.currency),
            Action::Sent => format!("I've sent {}.", terms.what_sends(who, deal)),
        }
    }
}

/// a step of the deal, in the order they are expected
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Amount,
    BitcoinMethod,
    PaymentMethod,
    BitcoinDetails,
    PaymentDetails,
    TakerSends,
    MakerSends,
}

const STEPS: [Step; 7] = [
    Step::Amount,
    Step::BitcoinMethod,
    Step::PaymentMethod,
    Step::BitcoinDetails,
    Step::PaymentDetails,
    Step::TakerSends,
    Step::MakerSends,
];

impl Step {
    /// who takes this step
    pub fn who(self, terms: &OfferTerms) -> Role {
        match self {
            Step::Amount | Step::BitcoinMethod | Step::PaymentMethod | Step::TakerSends => {
                Role::Taker
            }
            Step::BitcoinDetails => terms.bitcoin_receiver(),
            Step::PaymentDetails => terms.bitcoin_receiver().other(),
            Step::MakerSends => Role::Maker,
        }
    }

    fn of(action: &Action, who: Role) -> Step {
        match action {
            Action::Amount(_) => Step::Amount,
            Action::BitcoinMethod(_) => Step::BitcoinMethod,
            Action::PaymentMethod(_) => Step::PaymentMethod,
            Action::BitcoinDetails(_) => Step::BitcoinDetails,
            Action::PaymentDetails(_) => Step::PaymentDetails,
            Action::Sent if who == Role::Taker => Step::TakerSends,
            Action::Sent => Step::MakerSends,
        }
    }
}

/// what has been agreed so far. every value, once set, stays: there are no take-backs, a deal
/// that went wrong is talked over in the chat or abandoned.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Deal {
    pub amount: Option<f64>,
    /// an empty string when the offer didn't say, then it is agreed in the chat
    pub bitcoin_method: Option<String>,
    pub payment_method: Option<String>,
    pub bitcoin_details: Option<String>,
    pub payment_details: Option<String>,
    pub taker_sent: bool,
    pub maker_sent: bool,
}

impl Deal {
    /// whatever the offer leaves no choice about is decided from the start
    pub fn new(terms: &OfferTerms) -> Self {
        let only = |options: &[String]| match options {
            [] => Some(String::new()),
            [one] => Some(one.clone()),
            _ => None,
        };
        Deal {
            amount: terms.fixed_amount().then_some(terms.amount_min),
            bitcoin_method: only(&terms.bitcoin_methods),
            payment_method: only(&terms.payment_methods),
            ..Default::default()
        }
    }

    /// apply the actions of the chat in order, ignoring the ones that make no sense
    pub fn replay(terms: &OfferTerms, messages: &[Message]) -> Self {
        let mut deal = Deal::new(terms);
        let me = terms.my_role();
        for m in messages {
            if let Some(action) = m.action.as_deref().and_then(Action::from_tag) {
                deal.apply(terms, if m.mine { me } else { me.other() }, &action);
            }
        }
        deal
    }

    pub fn done(&self, step: Step) -> bool {
        match step {
            Step::Amount => self.amount.is_some(),
            Step::BitcoinMethod => self.bitcoin_method.is_some(),
            Step::PaymentMethod => self.payment_method.is_some(),
            Step::BitcoinDetails => self.bitcoin_details.is_some(),
            Step::PaymentDetails => self.payment_details.is_some(),
            Step::TakerSends => self.taker_sent,
            Step::MakerSends => self.maker_sent,
        }
    }

    /// whether everything a step depends on is done
    pub fn ready(&self, step: Step) -> bool {
        let d = |s| self.done(s);
        match step {
            Step::Amount | Step::BitcoinMethod | Step::PaymentMethod => true,
            Step::BitcoinDetails => d(Step::Amount) && d(Step::BitcoinMethod),
            Step::PaymentDetails => d(Step::Amount) && d(Step::PaymentMethod),
            Step::TakerSends => STEPS[..5].iter().all(|s| d(*s)),
            Step::MakerSends => d(Step::TakerSends),
        }
    }

    /// the steps not done yet, in order
    pub fn pending(&self) -> Vec<Step> {
        STEPS.into_iter().filter(|s| !self.done(*s)).collect()
    }

    /// the steps `who` can take right now
    pub fn available(&self, terms: &OfferTerms, who: Role) -> Vec<Step> {
        self.pending()
            .into_iter()
            .filter(|s| s.who(terms) == who && self.ready(*s))
            .collect()
    }

    pub fn concluded(&self) -> bool {
        self.maker_sent
    }

    /// why `who` can't do `action` now, if they can't
    pub fn check(&self, terms: &OfferTerms, who: Role, action: &Action) -> Result<(), String> {
        let step = Step::of(action, who);
        if step.who(terms) != who {
            return Err("that is up to the other side".into());
        }
        if self.done(step) {
            return Err("that was already decided".into());
        }
        if !self.ready(step) {
            return Err("that comes later".into());
        }
        match action {
            Action::Amount(a) => {
                // compare what people see, so rounding doesn't reject the max itself
                let cents = |v: f64| (v * 100.0).round() as i64;
                if cents(*a) < cents(terms.amount_min) || cents(*a) > cents(terms.amount_max) {
                    return Err(format!(
                        "the amount must be between {} and {} {}",
                        fiat(terms.amount_min),
                        fiat(terms.amount_max),
                        terms.currency
                    ));
                }
            }
            Action::BitcoinMethod(m) if !terms.bitcoin_methods.contains(m) => {
                return Err(format!("{m} is not one of the offer's bitcoin methods"));
            }
            Action::PaymentMethod(m) if !terms.payment_methods.contains(m) => {
                return Err(format!("{m} is not one of the offer's payment methods"));
            }
            Action::BitcoinDetails(d) | Action::PaymentDetails(d) if d.trim().is_empty() => {
                return Err("the details are empty".into());
            }
            _ => {}
        }
        Ok(())
    }

    /// returns whether it was applied
    pub fn apply(&mut self, terms: &OfferTerms, who: Role, action: &Action) -> bool {
        if self.check(terms, who, action).is_err() {
            return false;
        }
        match action {
            Action::Amount(a) => self.amount = Some(*a),
            Action::BitcoinMethod(m) => self.bitcoin_method = Some(m.clone()),
            Action::PaymentMethod(m) => self.payment_method = Some(m.clone()),
            Action::BitcoinDetails(d) => self.bitcoin_details = Some(d.trim().to_string()),
            Action::PaymentDetails(d) => self.payment_details = Some(d.trim().to_string()),
            Action::Sent if who == Role::Taker => self.taker_sent = true,
            Action::Sent => self.maker_sent = true,
        }
        true
    }

    /// what has to happen next, from `terms.my_role()`'s point of view
    pub fn next_step(&self, terms: &OfferTerms) -> String {
        let me = terms.my_role();
        let Some(step) = self.pending().into_iter().next() else {
            return "both sides have sent their part. check that you got yours, then conclude the trade."
                .into();
        };
        let mine = step.who(terms) == me;
        let what = match step {
            Step::Amount => "pick the amount".to_string(),
            Step::BitcoinMethod => "pick how the bitcoin is sent".into(),
            Step::PaymentMethod => format!("pick how the {} is sent", terms.currency),
            Step::BitcoinDetails => format!(
                "give {} {}",
                if mine { "your" } else { "their" },
                destination_name(self.bitcoin_method.as_deref().unwrap_or(""))
            ),
            Step::PaymentDetails => format!(
                "give {} {} details",
                if mine { "your" } else { "their" },
                self.payment_method.as_deref().unwrap_or("payment")
            ),
            Step::TakerSends | Step::MakerSends => {
                format!("send {}", terms.what_sends(step.who(terms), self))
            }
        };
        if mine {
            format!("your turn: {what}")
        } else {
            format!("waiting for them to {what}")
        }
    }
}

/// how many satoshis a fiat amount buys at `price` (fiat per bitcoin)
pub fn sats(amount: f64, price: f64) -> u64 {
    (amount / price * 100_000_000.0).round() as u64
}

/// what to ask the bitcoin receiver for, depending on how it is sent
pub fn destination_name(bitcoin_method: &str) -> &'static str {
    match bitcoin_method {
        "lightning" => "lightning invoice",
        "onchain" | "liquid" | "rootstock" | "arkade" => "address",
        _ => "address or invoice",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(mine: bool) -> OfferTerms {
        OfferTerms {
            mine,
            side: Side::Sell,
            currency: "USD".into(),
            price_api: "coingecko".into(),
            premium: 1.0,
            amount_min: 20.0,
            amount_max: 100.0,
            payment_methods: vec!["Zelle".into(), "Strike".into()],
            bitcoin_methods: vec!["lightning".into()],
        }
    }

    fn msg(mine: bool, action: Action) -> Message {
        Message {
            mine,
            action: Some(action.to_tag()),
            ..Default::default()
        }
    }

    #[test]
    fn tags_roundtrip() {
        for a in [
            Action::Amount(50.5),
            Action::BitcoinMethod("lightning".into()),
            Action::PaymentMethod("PIX".into()),
            Action::BitcoinDetails("lnbc1…".into()),
            Action::PaymentDetails("iban\nname".into()),
            Action::Sent,
        ] {
            assert_eq!(Action::from_tag(&a.to_tag()), Some(a));
        }
        assert_eq!(
            Action::from_tag(&["trade".into(), "amount".into(), "-1".into()]),
            None
        );
    }

    #[test]
    fn sats() {
        assert_eq!(super::sats(50.0, 100_000.0), 50_000);
        assert_eq!(super::sats(1.0, 3.0), 33_333_333);
    }

    #[test]
    fn single_options_are_decided() {
        let d = Deal::new(&terms(false));
        assert_eq!(d.amount, None);
        assert_eq!(d.bitcoin_method.as_deref(), Some("lightning"));
        assert_eq!(d.payment_method, None);
    }

    #[test]
    fn a_whole_trade_from_both_sides() {
        // the maker sells: the taker receives bitcoin and sends fiat first
        let chat = |taker_view: bool| {
            let t = !taker_view; // "mine" from the maker's point of view
            vec![
                msg(!t, Action::Amount(150.0)), // out of range, ignored
                msg(!t, Action::Amount(50.0)),
                msg(t, Action::Amount(60.0)), // the maker can't pick it
                msg(!t, Action::PaymentMethod("Zelle".into())),
                msg(t, Action::Sent), // too early
                msg(!t, Action::BitcoinDetails("lnbc1".into())),
                msg(t, Action::PaymentDetails("me@zelle".into())),
                msg(!t, Action::Sent),
                msg(t, Action::Sent),
            ]
        };
        let taker = Deal::replay(&terms(false), &chat(true));
        let maker = Deal::replay(&terms(true), &chat(false));
        assert_eq!(taker, maker);
        assert_eq!(taker.amount, Some(50.0));
        assert_eq!(taker.payment_details.as_deref(), Some("me@zelle"));
        assert!(taker.concluded());

        // halfway, the maker gives their zelle while the taker gives the invoice
        let half = Deal::replay(&terms(true), &chat(false)[..5]);
        assert_eq!(
            half.available(&terms(true), Role::Maker),
            vec![Step::PaymentDetails]
        );
        assert_eq!(
            half.available(&terms(false), Role::Taker),
            vec![Step::BitcoinDetails]
        );
        assert_eq!(
            half.next_step(&terms(false)),
            "your turn: give your lightning invoice"
        );
    }
}

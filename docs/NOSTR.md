# bitcoin-smol protocol

How bitcoin-smol uses nostr. Implementation lives in `../src/protocol.rs` (event kinds, offer
encoding, the DM scheme, pings, reviews), with prices in `../src/prices.rs`, the deal state in
`../src/deal.rs` and trust queries in `../src/store.rs`.

Event kinds used:

| kind    | what                                              |
| ---     | ---                                               |
| `6012`  | offer                                             |
| `8021`  | DM, encrypted with the conversation key           |
| `28022` | ping: "there is a DM for you from me"             |
| `8022`  | trade review: success                             |
| `8020`  | trade review: failure                             |
| `5`     | NIP-09 deletion, to close an offer before expiry  |

## Identity

The app generates a nostr secret key on first launch and stores it on the device.
Your npub is your only identity. Offers carry no username or profile, only the npub that signed them. You can reveal the
nsec in Settings to back it up, or import an nsec you used before. Importing is refused while there are open offers or
ongoing trades, since those belong to the current key. The replaced key is kept as `key.<pubkey>.old`.

## Offers (kind `6012`)

An offer is a regular event of kind `6012`. Its `content` is a free-form description, such as hours or
conditions. The terms go in tags:

```jsonc
{
  "kind": 6012,
  "content": "I'm usually online in the evenings",
  "tags": [
    ["side", "sell"],                  // the maker "sell"s or "buy"s bitcoin
    ["currency", "USD"],               // fiat currency to exchange for bitcoin
    ["price_api", "coingecko"],        // where the market price comes from
    ["premium", "+1%"],                // price relative to the market price, absent means "+0%"
    ["amount", "20", "100"],           // min and max, in the fiat currency
    ["payment", "Strike"],             // payment methods, one tag each, free-form
    ["payment", "Zelle"],
    ["bitcoin", "lightning"],          // how the bitcoin side is settled, one tag each
    ["bitcoin", "liquid"],
    ["relay", "wss://nostr.wine/"],    // where the DM negotiation happens, one or more
    ["relay", "wss://nostr.land/"],
    ["expiration", "1791345600"]       // NIP-40, at most 3 hours after created_at
  ]
}
```

- **`side`** isn't in the original list of terms, but an offer means nothing without it. It is always from the maker's
  point of view.
- **`currency`**: the app hardcodes a list of currencies (USD, EUR, GBP, MXN, ARS, VES, COP, …).
- **`price_api`**: each currency lists the APIs that can quote it, and the app has a fetch function for each:
  `coingecko`, `yadio`, `kraken`, `bitstamp` and `mempool` (see `../src/prices.rs`). Clients that don't know an API id
  show the offer with an "unknown price api" warning.
- **`premium`**: the actual price is a variation over the market price. `"+1%"` means bitcoin is bought or sold 1% more
  expensive than the market price, and `"-2%"` means 2% cheaper. If the tag is absent, the premium is 0%.
- **`amount`**: how much of the fiat currency the maker trades, anything from the min to the max. Both are positive
  decimal numbers and min can't be more than max; equal values (or a single value) mean a fixed amount. Offers
  without it are ignored.
- **`payment`**: free-form. The app offers a hardcoded list similar to RoboSats (Revolut, Wise, Zelle, Strike, SEPA,
  UPI, Mercado Pago, M-Pesa, gift cards, Cash F2F, …) plus a text field for anything else.
- **`bitcoin`**: how the maker accepts or sends bitcoin. The app offers `onchain`, `rootstock`, `lightning`, `cassis`,
  `arkade` and `liquid`, and requires at least one. These only show in the focused offer view, not in the offer list.
- **`relay`**: the relays where the taker and maker exchange DMs about this offer.
- **`expiration`**: offers live for at most 3 hours. Clients only fetch offers from the last 3 hours
  (`since: now - 3h`) and discard any offer whose expiration has passed or is more than 3 hours after its `created_at`.
  To keep an offer up, publish it again.

The app remembers which relays accepted each of your offers. Closing an offer before it expires sends a NIP-09
deletion (kind `5`, with `["e", <offer id>]` and `["k", "6012"]`) to those relays.

## Relays

The app uses a set of "app relays" to discover and publish new offers and trade reviews. By default this is only `nostr.wine`, a paid relay, so identities are not free. The Settings menu lets you switch relays on and off, remove them, and add your own.

## DMs: a shared-key scheme (kinds `8021` and `28022`)

This is a new scheme, not NIP-17 or NIP-04.

1. Both parties compute the same **conversation key** from NIP-44:
   `conversation_key = hkdf_extract(salt="nip44-v2", ecdh(my_sk, their_pk))`.
   Each trade has a **trade id**: an arbitrary string (the app uses 32 random hex characters, at most 64 characters
   are accepted) that the taker makes up and sends in the ping. It is mixed in so that two trades between the same
   two parties, even about the same offer, never share a conversation:
   `conversation_key = sha256("smol-trade:" || hkdf_extract(...) || trade_id)`. Without a trade id (pings of older
   versions) the key is the plain NIP-44 one.
   The key is 32 bytes, so it is also a valid secp256k1 secret key. Its public key is the **conversation pubkey**.
2. A message starts as an unsigned kind `14` **rumor**, as in NIP-17/NIP-59:
   `{id, pubkey: <real author>, created_at, kind: 14, tags: [["p", <recipient>], ["t", <trade id>], ["e", <offer id>]], content}`.
3. The rumor is NIP-44-encrypted with the conversation key. The result goes into the `content` of a kind `8021` event,
   which is **signed with the conversation key**. So the event's `pubkey` is the conversation pubkey. Its only tag is a
   NIP-40 `["expiration", <created_at + 30 days>]`.
4. That event is published to the relays from the offer announcement.

Consequences:

- **Deniability.** Either party could have signed any message, since both know the conversation key. No message can
  be provably attributed to either side, not even to the other party in the conversation. The only way to know which
  messages are yours is to remember them, which the app does by keeping its own rumors in its local database (see
  [Local storage](#local-storage-of-trades)). Any other message in the conversation is from the other party.
- **Opt-in listening and instant blocking.** To receive messages from someone you subscribe to
  `{"kinds": [8021], "authors": [<conversation pubkey>]}`. You only see DMs from people you chose to listen to.
  Blocking someone just means not making that subscription anymore. Their messages never reach you, and the relay
  doesn't know who you blocked.
- **No public metadata.** Relays and observers only see a pubkey nobody has seen before, posting encrypted blobs. They
  can't tell which two people are behind it.

### Pings (kind `28022`)

The maker can't subscribe to a conversation pubkey without knowing who the taker is. So to start a conversation, the
taker sends a **ping**: a kind `28022` event signed with the taker's real key, whose tags are `["p", <maker>]` and
`["trade", <trade id>]`. It means "look, there is a DM for you from me." It goes to the offer's relays.

The maker's app listens for `{"kinds": [28022], "#p": [<me>]}` on the app relays and on the negotiation relays of the
maker's recent offers. When a ping arrives from someone who isn't blocked, the app derives the conversation pubkey from the
trade id and subscribes to it. A ping with a new trade id is a new trade, even from someone it is already trading with. Pings from blocked npubs are ignored.

Kind `8021` is a regular event, so relays store DMs and the other side doesn't need to be online when you send one.
Each DM expires 30 days after it was sent: relays that support NIP-40 drop it then, and clients
ignore it. Messages the app has already received stay in its local cache.

Kind `28022` is in the ephemeral range, so relays forward pings to live subscribers but don't store them. Until the
other side has replied, the app sends a fresh ping with each message and waits a moment before publishing the message.
That gives the other side time to subscribe.

## Local storage of trades

The app keeps nothing about trades outside of its local event database (`events.redb`). A trade is put back together from
the events that refer to its trade id, with a `["t", <trade id>]` tag:

- **The ping** that started it, saved when the trade starts (the one we sent as the taker, or the first one we got as the
  maker). Pings carry the id in a `trade` tag, so at startup the app reads the last 200 pings to find the last 200 trades.
- **The rumors** of the conversation, decrypted, never the encrypted events. A rumor has no id check, signature or
  trustworthy author, so the app saves it as a kind `14` event whose `pubkey` is whoever it knows wrote it (us or the
  peer), with the `t` tag added, its id computed again and a blank signature.
- **Marks**, kind `8023`, notes to ourselves that are never published, signed by nobody. Their tags are
  `["p", <peer>]`, `["t", <trade id>]` and `["mark", <name>]`, and the names are: `offer` (with `["e", <offer id>]`
  and, as content, the terms of the offer, which will be gone from the relays by the time we look again),
  `price-check` (the price we expected, as content), `concluded` and `archived`.
- **Our reviews** of the trade, which are the public events above.

Where a trade stands comes from the last of its `concluded`, `archived` marks and our own reviews. A trade with none of them
is ongoing, one concluded and not reviewed is waiting for its review, and one with a review is reviewed.

## Structured trade messages

Some messages in a trade's chat are made by buttons, not typed. They are ordinary rumors whose `content` says in
plain English what happened, and which carry the same thing as data in a `trade` tag:

```jsonc
["trade", "amount", "50"]                  // "Let's trade 50 USD."
["trade", "price", "61234.56"]             // "The price is 61,234 USD/BTC, so 50 USD is 81,650 sats."
["trade", "bitcoin-method", "lightning"]   // "Let's send the bitcoin via lightning."
["trade", "payment-method", "Zelle"]       // "Let's send the USD via Zelle."
["trade", "bitcoin-details", "lnbc1…"]     // "Send the bitcoin to: …", from whoever receives the bitcoin
["trade", "payment-details", "me@zelle"]   // "Send the USD to: …", from whoever receives the fiat
["trade", "sent"]                          // "I've sent the 50 USD." / "I've sent the bitcoin."
```

Both sides replay these, in order, on top of the offer's terms, to get the same state of the deal (`../src/deal.rs`).
The taker picks the amount and the methods among what the offer allows. A fixed amount is sent as soon as the taker
contacts the maker, and methods the offer leaves no choice about are decided from the start. The maker's app answers
the amount with the price of a bitcoin: the market price from the offer's `price_api` plus its `premium`, at that
moment. That price is the one the trade uses. The taker's app checks it against its own fetch from the same source and
flags it when it is more than 1% off. Then each side gives the details for what they receive, the taker sends their part first and
says so, and the maker sends theirs after that. Each value is set once. Actions from the wrong side, out of order,
or outside the offer's terms are ignored. The maker learns which offer the trade is about from the `e` tag on the
taker's messages.

## Trade reviews (kinds `8022` and `8020`)

When a trade is over, either party can publish a review about the other:

```jsonc
{ "kind": 8022, "tags": [["p", "<other party>"]], "content": "$5 via UPI, fast" }  // success
{ "kind": 8020, "tags": [["p", "<other party>"]], "content": "never paid" }        // failure
```

A review about a trade also carries its trade id, `["t", "<trade id>"]`, which is how the app finds the reviews of a
trade. A review that is not about a trade, like one written from the person's profile, has no such tag.

Reviews are signed with your real key. The app stores them in a local event database and publishes them to your app relays and your write relays.

## Deciding who to trust

When you open someone's profile, the app shows:

1. **Your history with them**: every `8020` or `8022` that *you* wrote about them, read from the local database.
2. If you have fewer than 20 of those, **what the people you trust say**. The app ranks the people you have reviewed
   successful trades with (your own `8022`s) by number of reviews, takes the top 10, and asks your relays for
   `{"kinds": [8020, 8022], "authors": [<those 10>], "#p": [<this person>]}`. It shows any results with their authors
   and caches them locally.

So there is no global score that can be gamed. Everything is local and relative: the more you trade, the bigger your
web of people whose opinion you can rely on. A newcomer with no history has to start small, and so does anyone dealing
with a newcomer.

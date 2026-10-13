# bitcoin-smol

A tiny peer-to-peer market for exchanging bitcoin and fiat over nostr.

There is **no escrow, no arbiter, no usernames and no server**. Every trade is based on trust alone, so smaller amounts are encouraged.

<p>
  <img src="docs/static/screenshots/market.png" width="260" alt="the market">
  <img src="docs/static/screenshots/trade.png" width="260" alt="a trade in progress">
  <img src="docs/static/screenshots/profile.png" width="260" alt="a profile with trade reports">
</p>

📖 **User guide:** <https://andunieee.github.io/bitcoin-smol/>

## How it works in a nutshell

- Makers post offers to buy or sell bitcoin for some fiat amount, at the market price plus a premium, over whatever payment methods they accept.
- Takers answer over encrypted messages, scoped to that trade specifically.
- It's not raw chat: structured messages guide the flow, speed up the process and prevent ambiguity in the communication.
- The taker sends the fiat transfer first, if the maker is selling Bitcoin -- or the taker sends the Bitcoin if the maker is buying.
- After a trade, report how it went. Your reports build a web of trust and help ostracize bad actors and promote honest actors, helping yourself and others.

## Getting started

1. **Get the app:** download the desktop or Android app from the [releases page](https://github.com/andunieee/bitcoin-smol/releases/latest), or open the [web version](https://andunieee.github.io/bitcoin-smol/app/) in your browser.
3. Filter by your desired currency and **take an offer** from the orderbook, or **make your own**. The chat page should guide you through what should happen in each step.
4. **Conclude the trade** when it is done to publish a report about the other person.

Starting with someone you don't trust? Keep the amount to something small you're comfortable to lose, like $2.
See [Trading safely](https://andunieee.github.io/bitcoin-smol/safety/).

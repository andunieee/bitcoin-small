+++
title = "Taking an offer"
weight = 3
[extra]
nav = "take"
+++

The **Market** tab lists offers posted in the last 3 hours. Filter them by currency, and by whether the person wants to buy or sell bitcoin.

Each offer shows:

- **the price**: the market price plus or minus a premium. `+1%` means the bitcoin costs 1% more than the market
  price, and `-1%` means 1% less. The app shows where that market price comes from.
- **payment**: the ways the other person can send or receive the money, like UPI, Venmo, PIX, a bank transfer, Revolut or cash.
- **bitcoin via**: how the bitcoin side is sent, like lightning, on-chain or liquid.
- **a description**: limits, hours or conditions, in the maker's own words.

<div class="shots">
  <img src="../screenshots/market.png" alt="the market">
  <img src="../screenshots/offer.png" alt="an offer, opened">
</div>

## Before you contact anyone

Open the offer and check **the maker's trade history**. It shows what you said about them in past trades, and what the people you trust said about them. See [Trading safely](../safety/) for how to read it.

No history isn't a red flag in itself: everyone starts with none. It just means you should start very small.

## Contact and agree

Tap **Buy Bitcoin from …** (or **Sell Bitcoin to …**) to open a private chat. Next to it, the trade shows what has been agreed so far and what has to happen next:

1. you pick the amount, and how the bitcoin and the money are sent, among what the offer allows,
2. each of you gives the details for what you receive, like the bank account or the lightning invoice. Check them twice before sending: wrong details can mean money,
3. **you, the taker, send first**, and tap the button saying so. Then the maker sends their part. Which of the two you send depends on the offer — see [who sends first](#who-sends-first).

Each of these sends a message in the chat, so the other side sees it too.

<div class="shots one">
  <img src="../screenshots/trade.png" alt="a trade where everything is agreed and it is the taker's turn to send the money">
</div>

Once you pick the amount, the maker's app answers with the price: the market price from the offer's source plus its premium. From then on that price is fixed for the trade, and it decides how many sats the amount is. Your app checks it against the same source and warns you if it is too far off. If it is, ask them about it before going any further.

Anything else you settle by chatting.

The trade appears under **Trades → Ongoing**. Once it's done, tap **Conclude**. The trade moves to **Pending reviews**, where it stays until you tap **Review** to say how it went, which publishes a short review about the other person and helps you and the people who trust you next time. There is no hurry, wait until you're sure. **Archive** puts the trade away without a review, from the ongoing trade or from pending reviews.

## Who sends first

It is always the taker who sends first. If the offer is for **selling** bitcoin, the you send the **fiat** first, and the maker sends the bitcoin once it has arrived. If the offer is for **buying** bitcoin, the you sends the **bitcoin** first, and the maker sends the fiat after.

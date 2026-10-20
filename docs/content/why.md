+++
title = "Why this exists"
weight = 1
[extra]
nav = "why"
+++

For bitcoin to become the money people use everywhere, people everywhere need a way to move between it and the money they are paid in and pay their bills with: pesos in the Philippines, naira in Nigeria, dollars, euros. Today that usually means an exchange, and an exchange wants your ID, your address and your bank details. It can freeze your account, it may not serve your country at all, and it reviews what you do to the enemies of Bitcoin.

## Compared to Bisq and RoboSats

[Bisq](https://bisq.network) and [RoboSats](https://learn.robosats.com) aim for full guarantees: an escrow holds the coins, and your identity stays private. The price is steep:

- **A fee, and a scammer on every trade.** Escrow assumes the person across from you may be trying to cheat you, so every trade is played as if they were.
- **A third party that can go down.** The escrow is a point of centralization and dependency. If it disappears, so do your guarantees.
- **Disputes are a gamble.** Scammers can trick the people resolving them, and honest users end up losing anyway.
- **Privacy that buys little.** In most trades you have to hand your bank details to the other side anyway.
- **Anxiety.** You follow a playbook of rules and careful verification at every step, and a slip can get you punished by the escrow. Reversals remain a risk regardless.

bitcoin-smol removes the escrow entirely. That gives up some guarantees when trading with complete strangers, but people who trade bitcoin online are a small group, and most of them are honest. A closed circle of reputable peers forms quickly. Once you have enough reputation and know other trustworthy contacts, the stress is gone and trading becomes a breeze. You are no longer at risk of being matched with a full-blown scammer.

## Compared to Vexl

[Vexl](https://vexl.it) is closer to bitcoin-smol: it also promotes trust-based trades. But it only lets you reach people you already know, directly or through someone else, via your phone contacts. That hurts adoption, and it keeps new honest people out, so liquidity doesn't improve for everybody.

bitcoin-smol welcomes newcomers. You trust them with a few dollars and let them prove themselves, trade by trade.

## What you get back from not having escrow guarantees

On an escrow platform, every trade is a trade with a possible scammer. Both sides stay wary, follow strict timeouts, use only the payment methods the platform accepts, keep receipts, and call in the platform's moderators when something goes wrong.

Once you trust someone, or someone you trust vouches for them with their reviews, none of that is needed. Trades just flow: you agree on the terms in a chat and do them.

Some frauds also get around escrow, like payment reversals or triangulation fraud, where a scammer has a third person pay you with money that is later pulled back. Here they are just another way of cheating someone, and they cost the scammer their reputation like any other.

And any way of paying works. Between people who trust each other, one can pay the other's electricity bill or buy something online for them, in exchange for bitcoin.

That makes bitcoin-smol a good fit for:

- getting your first few dollars of bitcoin,
- topping up or spending without involving a bank or an exchange,
- regular trades with people you have come to know.

## Other practical benefits of bitcoin-smol

- **No one to close shop.** It isn't tied to a company that can shut down tomorrow, a group of volunteers who must keep the infrastructure running, or a DAO that insists on its own token. It's an app and some nostr relays, and there are plenty of relays to pick from.
- **No account, no deposit.** You don't have to sign up with a provider or lock up a heavy deposit in escrow before
  you can even open an offer. You open the app and post it.
- **Light and fast.** It loads instantly and runs in the browser, on desktop and on phones. There is no heavy Java desktop app eating your computer's resources, so it works fine on a cheap phone or an old laptop.
- **No Tor needed.** Relays see your IP address, like any website. If you'd rather they didn't, a VPN is usually enough, and Tor works too. See [Privacy](../privacy/).
- **Easy to automate.** Everything, including each step of a trade, is a nostr message, and the steps carry structured data. So a maker can script the routine parts, or hand them to an LLM agent, instead of clicking buttons in an app all day. The fiat side may still need a human to check that the money arrived.

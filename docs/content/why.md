+++
title = "Why this exists"
weight = 1
[extra]
nav = "why"
+++

For bitcoin to become the money people use everywhere, people everywhere need a way to move between it and the money
they are paid in and pay their bills with: pesos in the Philippines, naira in Nigeria, dollars, euros. Today that
usually means an exchange, and an exchange wants your ID, your address and your bank details. It can freeze your
account, it may not serve your country at all, and it reports what you do.

Peer-to-peer markets solve part of that, but most of them still have a company or a server in the middle: something
that holds the coins in escrow, settles disputes and keeps a database of users. That middle part is what makes them
heavy, and it is also what can be shut down.

bitcoin-smol removes the middle entirely:

- **No sign-up.** The app creates a key for you on first launch. That key is your identity. There is no username,
  email or phone number.
- **No server.** Offers and messages travel over [nostr](https://nostr.com) relays, which are simple, replaceable
  message boards. You can switch relays whenever you want.
- **No escrow.** Nobody holds the coins during a trade. You and the other person agree on how to do it.
- **No global score.** Instead of a rating anyone can inflate, you see what *you* and the people *you* traded well
  with have said about someone.

## The price of having no middleman

Without escrow, the person who pays first can be cheated. The app doesn't pretend otherwise. While you are looking
for people to trade with, keep every trade so small that losing it doesn't matter, and be ready to lose a few dollars
before you find the good ones. [Trading safely](../safety/) explains how to pick them.

## What you get back

On an escrow platform, every trade is a trade with a possible scammer. Both sides stay wary, follow strict timeouts,
use only the payment methods the platform accepts, keep receipts, and call in the platform's moderators when
something goes wrong.

Once you trust someone, or someone you trust vouches for them with their reports, none of that is needed. Trades just
flow: you agree on the terms in a chat and do them.

Some frauds also get around escrow, like chargebacks or triangulation fraud, where a scammer has a third person pay
you with money that is later pulled back. Here they are just another way of cheating someone, and they cost the
scammer their reputation like any other.

And any way of paying works. Between people who trust each other, one can pay the other's electricity bill or buy
something online for them, in exchange for bitcoin.

That makes bitcoin-smol a good fit for:

- getting your first few dollars of bitcoin,
- topping up or spending without involving a bank or an exchange,
- regular trades with people you have come to know.

## Compared to Bisq, RoboSats and Vexl

bitcoin-smol sits somewhere between [Bisq](https://bisq.network) and [RoboSats](https://learn.robosats.com) on one
side and [Vexl](https://vexl.it) on the other.

Its goals are closest to Vexl's: you trade with people you trust, or with people that people you trust have traded
with. But Vexl finds them through your phone contacts, so it works best if you already know bitcoiners. bitcoin-smol
also works if you don't: you can meet strangers online and, trade by trade, find out which of them are trustworthy.

Finding strangers to trade with is what Bisq and RoboSats are for too. They do it by keeping trust and reputation out
of the picture on purpose, with escrow, deposits, arbitrators and timeouts, and that is why they are so cumbersome to
use. bitcoin-smol lets trust do that work instead.

It is also different in a few practical ways:

- **No one to close shop.** It isn't tied to a company that can shut down tomorrow, a group of volunteers who must
  keep the infrastructure running, or a DAO that insists on its own token. It's an app and some nostr relays, and
  there are plenty of relays to pick from.
- **Light and fast.** It loads instantly and runs in the browser, on desktop and on phones. There is no heavy Java
  desktop app eating your computer's resources, so it works fine on a cheap phone or an old laptop.
- **No Tor needed.** Relays see your IP address, like any website. If you'd rather they didn't, a VPN is usually
  enough, and Tor works too. See [Privacy](../privacy/).
- **Easy to automate.** Everything, including each step of a trade, is a nostr message, and the steps carry
  structured data. So a maker can script the routine parts, or hand them to an LLM agent, instead of clicking buttons
  in an app all day. The fiat side may still need a human to check that the money arrived.

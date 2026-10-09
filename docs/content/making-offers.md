+++
title = "Making an offer"
weight = 4
[extra]
nav = "make"
+++

Use the **New offer** tab when you want people to come to you.

## Before your first offer: pay the relays

Offers are published to the relays in **Settings → Relays**, and by default those are paid relays. They won't accept
your offer until your key has paid their one-time fee. This small fee is what makes identities cost something, and it
is the reason people take offers from paid relays more seriously (see [Trading safely](../safety/)).

1. Copy your npub from **Settings → Your key**.
2. Open the website of each relay you want to use, like [nostr.wine](https://nostr.wine) or
   [nostr.land](https://nostr.land), and pay for that npub.

You only do this once per key. One relay is enough to start. The relays you didn't pay for simply reject the offer,
and the app tells you how many relays took it ("offer published to 2/7 relays").

## The form

- **I want to sell / buy bitcoin for** a currency.
- **premium**: how far from the market price you trade. `+2%` means whoever buys from you pays 2% above market. Leave it
  empty to trade at the market price.
- **bitcoin sent via**: how you can send or receive the bitcoin. Pick at least one.
- **fiat payment methods**: how the money moves. Methods common for your currency are shown first, and you can type
  others.
- **relays for the negotiation** (under "custom price source, relays and description"): where people reach you about
  this offer. These don't have to be the paid relays: by default they are free ones, like relay.primal.net and
  nos.lol.
- **description** (in the same section): amounts, hours, anything else people should know. "Up to $10, evenings only"
  is a good start.

The form remembers what you picked in your last few offers, so the next offer is usually one tap away.

## After publishing

Your offer stays up for **3 hours**, then disappears on its own. To keep trading, publish it again. You can close it
earlier from **Trades → Your open offers**.

When someone answers, a new chat appears under **Trades → Ongoing**. Check their history before agreeing to anything,
exactly as you would when [taking an offer](../taking-offers/). You don't have to trade with everyone who writes to you.

## Your offer is public

Anyone can see your offer and your npub. They can't see your chats, or who you trade with. See
[Privacy](../privacy/).

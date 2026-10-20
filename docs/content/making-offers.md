+++
title = "Making an offer"
weight = 4
[extra]
nav = "make"
+++

Use the **New offer** tab when you want people to come to you.

## Where your offer goes

Offers are published to the relays in **Settings → Relays**. Because the default relays we use are paid, before publishing a new offer you'll have to go on their website, pay their fee and authorize your npub. You can copy your npub from **Settings → Your key**.

## The form

- **I want to sell / buy bitcoin for** a fiat currency.
- **premium**: how far from the market price you trade. `+2%` means whoever buys from you pays 2% above market. Leave it empty to trade at the market price.
- **amount**: how much of your currency you want to trade, from a minimum to a maximum. The maximum copies the minimum as you type, for a fixed amount; change it to accept a range.
- **bitcoin sent via**: how you can send or receive the bitcoin. Pick at least one.
- **fiat payment methods**: how the money moves. Methods common for your currency are shown first, and you can type others.
- **market price from** (under "custom price source, relays and description"): the price API the market price comes from. The premium is applied to this price, and the preview under the premium field shows the result. By default this uses one established online source.
- **relays for the negotiation** (in the same section): where people reach you about this offer. They can be different from the relays the offer is published to. It's not usually necessary to change these.
- **relays where to publish this offer** (in the same section): extra relays to send the offer to, on top of the relays in **Settings → Relays**. This can also be ignored.
- **description** (in the same section): hours, anything else people should know. "Evenings only" is a good start. It's recommended to leave this blank unless you have some very special trade conditions you have to specify.

The form remembers what you picked in your last few offers, so the next offer is usually one tap away.

<div class="shots one">
  <img src="../screenshots/new-offer.png" alt="the new offer form">
</div>

## After publishing

Your offer stays up for **3 hours**, then disappears on its own. To keep trading, publish it again. You can close it earlier from **Trades → Your open offers**.

When someone answers, a new chat appears under **Trades → Ongoing**. Check their history before agreeing to anything, exactly as you would when [taking an offer](../taking-offers/).

<div class="shots one">
  <img src="../screenshots/trades.png" alt="your open offers, ongoing trades and history">
</div>

## Who sends first

It is always the taker who sends first, so you always send second. If your offer is for **selling** bitcoin, the taker sends you the **fiat** first, and you send the bitcoin once it has arrived. If your offer is for **buying** bitcoin, the taker sends you the **bitcoin** first, and you send the fiat after..

## Filtering people

You don't have to trade with everyone who writes to you. Check the history you have with them and the reviews of the people you trust, exactly as you would when [taking an offer](../taking-offers/).

The taker's obligation to send first makes your role the safer: whatever they want from you, they put their part in before you put yours in.

But safer doesn't mean completely safe. If you are selling bitcoin and the fiat payment method can be reversed, like a bank transfer or PayPal that can be charged back, someone can take your bitcoin and claw the money back days later. If you're dealing with that possibilities you might want to be careful about the reputation of the taker before accepting to trade with them, or you may also make it clear to them that you will wait for a longer period before sending your part, perhaps have that in the [offer description](#the-form).

## Your offer is public

Anyone can see your offer and your npub.

+++
title = "Trading safely"
weight = 5
[extra]
nav = "safety"
+++

When you start, you trust no one and no one trusts you. Expect to lose a few dollars along the way. Think of it as the price of finding the people you will keep trading with.

## Start small, then grow

How much to risk on a trade depends on what you and the people you trust know about the other person:

- **Nobody you know has traded with them**: start with something like $2, an amount you wouldn't mind losing.
- **People you trust have good trades with them**: you can start higher, like $20 or $100.
- **You have a long history of good trades together**: after years of that, you can raise your limits a lot.

Where to stop is always your call.

## Why identities should cost something

If making a new identity is free, a scammer can take $20 from a few people, and once some of them report it, throw the key away and start over with a clean one. A good history only means something if starting over has a cost.

Paid relays are one way to add that cost: they only accept posts from keys that paid them a fee, and a high one-time fee makes throwing a key away expensive. Offers you see on such relays come from keys that paid it. Free relays let in offers from keys that cost nothing to create, so weigh a history there accordingly.

For now the only paid relay that fits the model of asking for a steep lifetime fee is https://nostr.wine, so that's the one we use and recommend by default.

In the future we might develop other methods of ensuring identities have a cost, based on research and experiments from projects like Joinmarket.

## People you already know

The best first peers are people you know from somewhere else: a friend, a friend of a friend, a meetup, an online community. Ask for their npub and trade with them first.

## Read the history

When you open someone's profile, you see:

1. **Your own history with them**: every report you wrote about them.
2. **What the people you trust say**: if you have fewer than 20 reports of your own about them, the app asks the 10 people you have reported the most successful trades with, and shows what they wrote about this person.

<div class="shots one">
  <img src="../screenshots/profile.png" alt="a profile: your own reports about them, then what the people you trust said">
</div>

This is why reporting matters. Every successful trade you conclude can bring someone into the circle and increase the amount of successful trades for everybody.

Reports from people outside the circle of people you know are ignored, so fake accounts praising each other are irrelevant, a scammer can't just pump up their reputation by creating an army of bots.

## When it goes wrong

Conclude the trade as a **failure** with a short comment. It warns the people who trust you. Then use **Block** on the person's profile: you won't see their offers or hear from them again.

## Labels

Give people a label from their profile, like _"bob the bizum guy"_. Only you see labels. The **People** tab lists everyone you labeled.

<div class="shots one">
  <img src="../screenshots/people.png" alt="the People tab">
</div>

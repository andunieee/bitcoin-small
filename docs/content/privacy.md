+++
title = "Privacy"
weight = 6
[extra]
nav = "privacy"
+++

## What is public

- **Your offers**: what you want to trade, your premium, payment methods, description and npub. They disappear after 3 hours (the app itself ignores offers older than 3 hours, but the relays you publish them to should also delete them after 3 hours if they implement the `"expiration"` tag support).
- **Your trade reviews**: who you traded with, whether it went well, and your comment. These stay up as they are a signal to others, but you also don't have to publish a review.
- **That you contacted someone.** When you contact a maker, your app sends a signal from your key to theirs so their app starts listening. Relays can see it, so they can tell that you reached out to that person, but not what you said. Assume anyone might also be listening to these signals. Only one of these signals is sent for each trade you start, not one for each message.

## What is private

- **Your chats.** They are encrypted, and the messages themselves don't show who wrote them. Relays that support expiration drop them after 30 days.
- **Who you block.** Blocking just means your app stops listening to someone. Nobody else is told.
- **Your labels.** They never leave your device.
- **Your trade history**, apart from the reviews you choose to publish.

## What is not hidden

- The relays you use can see your IP address, like any website. Use a VPN or Tor if that matters to you.
- The payment itself, a bank transfer for example, goes through the usual banking system with your real name. Pick payment methods with that in mind.
- Anyone you trade with learns whatever payment details you send them.

## Your partner can't prove what you wrote

Both sides of a chat use the same shared key, so either of you could have written any message in it. Your trade partner can read your messages, but can't cryptographically prove to anyone else that you wrote them (it could have been themselves writing them pretending to be you).

+++
title = "Getting started"
weight = 2
[extra]
nav = "start"
+++

## Install

Download the latest version from the [releases page](https://github.com/andunieee/bitcoin-smol/releases/latest):

- **Android**: the `.apk` file. Your phone will ask you to allow installing apps from your browser or file manager.
- **Linux, macOS, Windows**: the file for your system. On macOS and Linux you may need to mark it as executable
  before running it.

Or [use it in your browser](../app/), with nothing to install. Your key and your trades stay in that browser, so clearing
its site data erases them. Back up your key first (see below).

## Your key is your identity

On first launch the app creates a secret key and keeps it on your device. Its public half, your **npub**, is the
only thing other people see. You will find both under **Settings → Your key**.

Your reputation, the reports people write about you, is tied to that key. If you lose it, you start again from zero.

> Back it up: tap **Reveal nsec** in Settings and write it down somewhere safe. Anyone who has your nsec can act as
> you, so never share it, and never paste it into a website or chat.

If you had a key before, paste its nsec under **Settings → Reset identity**. Other people will recognize you and see
the reports written about you. Your trades, messages and the reports you wrote stay on the device they happened on,
so on a new device the app doesn't yet know who you trust. Close your open offers and end your ongoing trades first.

## Relays

Relays are the servers that carry offers, messages and reports. The app comes with a suggested list, which you can
change in **Settings → Relays**. Anyone you want to trade with needs to see the same relays you post to, so stick to
well-known ones.

Some relays are paid: they only accept posts from keys that paid them a fee. That makes a new identity cost
something, which matters for trust. See [Trading safely](../safety/).

## Next

Read [Trading safely](../safety/) before your first trade. Then [take an offer](../taking-offers/) or
[make your own](../making-offers/).

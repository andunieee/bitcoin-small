+++
title = "Getting started"
weight = 2
[extra]
nav = "start"
+++

## Install

Download the latest version from the [releases page](https://github.com/andunieee/bitcoin-smol/releases/latest):

- **Android**: the `.apk` file.
- **Linux, macOS, Windows**: the file for your system. On macOS and Linux you may need to mark it as executable before running it.

Or [use it in your browser](../app/), with nothing to install. Your key and your trades stay in that browser, so clearing its site data erases them.

## Your key is your identity

On first launch the app creates a secret key and keeps it on your device. Its public half, your **npub**, is the only thing other people see. You will find both under **Settings → Your key**.

Your reputation (the reports people write about you) is tied to that key. Your reputation is what gives people the trust to trade with you. If you lose it, your reputation starts again from zero.

So at the moment you start getting some reputation you should consider backing up your key.

> Back it up: tap **Reveal nsec** in Settings and write it down somewhere safe. Anyone who has your nsec can act as you, so never share it, and never paste it into a website or chat.

## Relays

Relays are the servers that carry offers, messages and reports. The app uses slightly different relays for browsing new offers and then for messaging and publishing reports.

The most important relays are those that display the offers in your home screen: we don't want an offer screen flooded with scammers, so by default bitcoin-smol relies solely on the https://nostr.wine relay, which charges a one-time fee for new accounts. This upfront cost makes it so it is not free for users to scam others out of some small amount, get bad ratings on their profile, then just throw away that identity and use a new one to scam again.

## Practice with XFK

The first time you open the bitcoin-smol app it loads the fake currency **XFK**. It comes with two bot traders that run inside the app. These exist so you can go through the process of taking an offer, specifying your parameters to the other side and finally concluding the offer and publishing a report.

It also works on the other side: if you open an offer on **XFK** you will get a buyer or seller that is just a bot trying to transact with you.

## Next

Read [Trading safely](../safety/) before your first trade. Then [take an offer](../taking-offers/) or [make your own](../making-offers/).

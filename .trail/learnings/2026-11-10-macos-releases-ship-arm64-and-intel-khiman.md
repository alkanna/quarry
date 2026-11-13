---
title: "macOS releases ship arm64 and Intel apps from two parallel jobs, with no universal app"
author: Khiman
date: 2026-11-10
branch: master
cost: { minutes: 3, tokens: 94609 }
id: l-791982bc
scope: repo
status: active
src: agent
touches:
  - ".github/workflows/release.yml"
  - bundle.sh
  - README.md
  - src/update.rs
---

## Decided
The release builds and notarizes the macOS arm64 and x86_64 apps in two parallel jobs. The universal app was dropped: the updater only downloads the two single-arch zips, and the universal one cost a third build and a third notarization. The release jobs cache compiled dependencies with rust-cache.

## Tried and ruled out
One macOS job building arm64, x86_64 and universal in a row: about 17 minutes, and the third notarytool upload got a 504 from Apple, which failed the whole job and made a rerun start from nothing.

## Watch out
Both macOS jobs run on macos-latest, so rust-cache needs `key: matrix.name` or they share one cache key. Apple's notary service sometimes answers 504; bundle.sh retries the submission up to three times.

In the code: .github/workflows/release.yml build matrix and "macOS app" step, bundle.sh notarytool loop, src/update.rs asset_name.

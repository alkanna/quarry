---
title: "Dev builds sign as the release app so one keychain Always Allow covers both, and the signing p12 must use the legacy format"
author: Khiman
date: 2026-11-10
branch: feat/macos-signing
cost: { minutes: 2, tokens: 21704 }
id: l-15697ca8
scope: repo
status: active
src: agent
touches:
  - ".cargo/run-signed.sh"
  - ".github/workflows/release.yml"
  - Makefile
  - README.md
  - bundle.sh
---

## Decided
With a Developer ID installed, cargo run signs the dev binary with identifier dev.quarry.app, the release app's bundle id. The keychain trusts by designated requirement (identifier plus team), so dev and release builds share one Always Allow per saved password. Dev signing skips the timestamp so it works offline.

## Watch out
A .p12 made by OpenSSL 3 (Homebrew) fails security import with "MAC verification failed (wrong password?)" even with the right password. Export with openssl pkcs12 -export -legacy, or /usr/bin/openssl. The release job imports the same p12, so the secret must be the legacy one.

In the code: .cargo/run-signed.sh, bundle.sh, release.yml secret MACOS_CERT_P12.

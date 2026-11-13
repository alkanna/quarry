---
title: "Sign macOS builds with the certificate hash, not its name, because the accented name does not match"
author: Khiman
date: 2026-11-10
branch: master
cost: { minutes: 38, tokens: 95625 }
id: l-5eb14e2c
scope: repo
status: active
src: agent
touches:
  - THIRD_PARTY_NOTICES.md
  - assets/engines/mariadb.png
  - assets/engines/mongodb.png
  - assets/engines/mysql.png
  - src/app/rail.rs
  - src/ui/complete.rs
  - src/ui/sql_complete.rs
  - bundle.sh
  - ".github/workflows/release.yml"
---

## Decided
QUARRY_SIGN_ID is the Developer ID certificate's SHA-1 hash, not its name. The release job already looks the hash up with security find-identity.

## Tried and ruled out
Passing "Developer ID Application: Khiman Louër (ZLB665Q4PA)": codesign said "no identity found", because the ë typed in the shell and the one in the keychain are different Unicode forms.

## Watch out
The first notarization from a new developer account took about 56 minutes; later ones take minutes. notarytool --wait just keeps waiting, it is not stuck.

In the code: bundle.sh (QUARRY_SIGN_ID, QUARRY_NOTARY_ARGS), .github/workflows/release.yml "macOS signing keychain" step.

---
title: The README has no CI or stars badge because CI only runs on labelled PRs
author: Khiman
date: 2026-11-10
branch: master
cost: { minutes: 8, tokens: 118769 }
id: l-e8ef461a
scope: repo
status: active
src: agent
touches:
  - README.md
  - docs/aggregate.png
  - docs/compare.png
  - docs/connection.png
  - docs/documents.png
  - docs/edit.png
  - docs/schema.png
  - docs/shell-attach.png
  - docs/sql-content.png
  - docs/sql-query.png
---

## Decided
The README badge row shows the latest release, total downloads, platforms and license, all from shields.io (the repo is public).

## Tried and ruled out
- CI status badge: CI runs only when a PR gets the `ci` label, so the badge would show no status or a stale one.
- Stars badge: the count is low, so it works against the project for now.
- Built with Rust badge: the tagline already says it.

## Watch out
Retake every README screenshot after a change visible in the rail, tab bar or status bar, since all of them show those parts.

In the code:
- README.md badge row under the tagline.
- Screenshot mapping and size are in CLAUDE.md ("To regenerate the README images").

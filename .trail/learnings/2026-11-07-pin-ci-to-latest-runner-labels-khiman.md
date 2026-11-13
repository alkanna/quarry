---
title: Pin CI to latest runner labels because retired macOS images broke the release
author: Khiman
date: 2026-11-07
standing: true
id: l-adb5cdd0
scope: repo
status: active
src: init-sessions
touches:
  - ".github/workflows/ci.yml"
---

## Decided
- The macos-14 image was retired, so the universal macOS release job never started and v0.11.0 shipped without macOS files. Linux stays on ubuntu-22.04 on purpose for glibc compatibility.
- In the code: Use macos-latest and windows-latest in .github/workflows/ci.yml and release.yml; keep ubuntu-22.04. Missing assets can be rebuilt with `gh workflow run Release -f tag=<tag>`. ci: commits do not trigger release-please.

_From past Claude sessions here, Nov 5._

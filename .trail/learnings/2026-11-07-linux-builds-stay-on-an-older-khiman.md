---
title: Linux builds stay on an older Ubuntu for glibc compatibility
author: Khiman
date: 2026-11-07
standing: true
id: l-256ccaf9
scope: repo
status: active
src: init-review
---

## Decided
- CI and release moved macOS and Windows to latest runners after a retired image broke a release. Linux was deliberately kept pinned so binaries run on older glibc.
- In the code: Keep Linux on ubuntu-22.04 in CI and release workflows (glibc 2.35+). Use macos-latest and windows-latest for the other targets.

_From PR #57 here, Nov 6._

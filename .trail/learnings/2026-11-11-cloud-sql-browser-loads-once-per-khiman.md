---
title: "Cloud SQL browser loads once per session and the UI test feeds gcloud answers through Msg::CloudSql"
author: Khiman
date: 2026-11-11
branch: feat/pg-cloudsql-ui
cost: { minutes: 14, tokens: 123849 }
id: l-300d6635
scope: repo
status: active
src: agent
touches:
  - src/cloudsql.rs
---

Decided: the Add from Cloud SQL browser (design C) is its own window over the page or over the connection form, so Change in the form reopens it and Use fills that same form. gcloud answers live in App.cloud for the whole session; cloud_load(false) does nothing once cloud.req is set, Refresh calls cloud_load(true). Instance lists run at most 6 gcloud processes at once, because gcloud is a heavy Python program and an account can see many projects. Demo mode never runs gcloud (cloud_load returns on self.demo) and seeds the cache from recorded JSON through parse_instances.
Watch out: a UI test must set app.cloud.req before opening the browser, or the real gcloud on the machine runs. Feed Found::Instances before Found::Start: projects that already answered are not asked again. The stub proxy for UI tests is the test binary itself: cloudsql::open sees exe == current_exe under cfg(test) and runs the stub, since libtest would refuse the proxy's flags; cloudsql::tests::STUB_TARGET makes it forward to a live PostgreSQL.
Ruled out: deriving STOPPED only from state. gcloud's list already rewrites state to STOPPED when settings.activationPolicy is NEVER (DatabaseInstancePresentation in the SDK), and the raw API keeps RUNNABLE, so parse_instances reads both.

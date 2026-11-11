---
title: Cloud SQL IAM signs in with Application Default Credentials and the proxy listens on a private unix socket
author: Khiman
date: 2026-11-11
branch: feat/pg-cloudsql-fix
cost: { minutes: 422, tokens: 1010036 }
id: l-a1f22ee4
scope: repo
status: active
src: agent
touches:
  - src/cloudsql.rs
  - src/app/cloud_sql.rs
  - src/db/sql/pg.rs
  - src/app/poll.rs
---

Decided: IAM sign-in always uses Application Default Credentials, because cloud-sql-proxy refuses --gcloud-auth together with --auto-iam-authn ("cannot use --auto-iam-authn with --gcloud-auth. Instead use Application Default Credentials"). proxy_args adds --gcloud-auth only for a password sign-in when gcloud has an account. cloudsql::open refuses an IAM connect when adc_exists() finds no file (GOOGLE_APPLICATION_CREDENTIALS, else application_default_credentials.json in CLOUDSDK_CONFIG, %APPDATA%\gcloud or ~/.config/gcloud), and the browser and the failure card show Card::NoAdc. The form still fills the IAM user from the gcloud account, which can differ from the ADC account.
Decided: on macOS and Linux the proxy gets one exact socket, instance?unix-socket-path=<dir>/.s.PGSQL.5432 (query-encoded; the proxy url-decodes it, checked with port=5%3557 giving 5557), in a fresh 0700 folder removed with CloudProxy; sqlx connects with PgConnectOptions::socket(dir) and port 5432. A localhost port could be squatted between free_port() and the proxy binding it, and a TCP connect check trusted the squatter. Windows keeps TCP (sqlx has no unix socket there) and waits for the proxy's stdout line "ready for new connections" before the connect check.
Watch out: never pass --token (ps shows it). The stub proxy (cloudsql::tests::stub_proxy) listens on QUARRY_STUB_SOCKET or QUARRY_STUB_PORT and prints the ready line; mode squat listens without it. Connect answers carry an attempt id, and poll drops one unless that connection is still Connecting for it, which also kills a proxy the user disconnected. update::restart exits without destructors, so App::restart drops every Way first.

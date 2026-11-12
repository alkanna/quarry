---
title: "PostgreSQL verify-ca and verify-full through SSH open TLS in a Quarry relay, not sqlx"
author: Khiman
date: 2026-11-12
branch: fix/pg-verify-ca
cost: { minutes: 527, tokens: 1279885 }
id: l-4cc88062
scope: repo
status: active
src: agent
touches:
  - src/db/sql/pg.rs
  - src/ssh.rs
  - src/db/sql/worker.rs
  - src/db/errors.rs
---

Cause: sqlx 0.8.6's NoHostnameTlsVerifier (sqlx-core net/tls/tls_rustls.rs) forgives only CertificateError::NotValidForName, and rustls 0.23.24+ returns NotValidForNameContext, so sqlx verify-ca checked the host name like verify-full, for PostgreSQL and MySQL alike; and sqlx takes the TLS name from the host it dials, so through the SSH tunnel's 127.0.0.1 every ordinary certificate failed (the old downgrade of verify-full to verify-ca did not help). Fixed only in sqlx 0.9.0 (#3861); no 0.8.x patch, and 0.9 breaks every dynamic query (SqlSafeStr) and MySQL collations, so it was not taken. Decided: pg_tls::start runs a relay on a socket in a fresh 0700 folder (localhost port on Windows) for verify-ca and for verify-full through SSH; sqlx connects with sslmode disable, the relay sends SSLRequest and runs rustls with WebPkiServerVerifier, forgiving only the name error for verify-ca (rustls checks the chain first), against the host typed, with the client certificate. Roots as libpq, not sqlx (security review): the CA file alone when set, in verify-ca and verify-full, so verify-full with a CA file goes through the relay too; verify-ca without a file is refused before connecting; webpki-roots only for verify-full without a file. Web roots plus a forgiven name would accept any public site's certificate. On Windows the relay's localhost port serves only sockets of Quarry's own pid (GetExtendedTcpTable), failing closed. A failed handshake is sent to sqlx as FATAL 08001 after reading its startup message (closing with unread data resets the socket and can lose the error), and open() swaps in the relay's text (Relay::explain, only when sqlx's error carries it) so the first error reads exactly like sqlx's (an 08 code alone would make classify say Lost). The relay is held by PgPools. Ruled out for MySQL: a relay, because MySQL negotiates TLS inside its handshake (sequence ids, caching_sha2 RSA path). Instead ssh::open also listens on a private unix socket when MySQL asks (Verify CA or identity; a socket error falls back to the port), and sqlx-mysql dials it with host = the name typed, since sqlx does TLS over unix sockets with options.host as the name; both accept loops run in the tunnel's one task so abort stops both. MySQL Verify CA still checks the name (mysql_name_note says so); on Windows the name checked through SSH is 127.0.0.1. Live test: tls_tests with the containers in docs/sql/POSTGRES.md Verification.

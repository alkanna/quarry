---
title: "PostgreSQL client certificates are read and decrypted by Quarry, not sqlx"
author: Khiman
date: 2026-11-12
branch: feat/pg-certs
cost: { minutes: 475, tokens: 1156156 }
id: l-afdb458f
scope: repo
status: active
src: agent
touches:
  - src/db/sql/pg.rs
  - src/db/errors.rs
---

Decided: sqlx 0.8 (rustls) takes sslcert and sslkey as files or PEM bytes but has no sslpassword, and rustls-pki-types reads only unencrypted PKCS#1, PKCS#8 and SEC1 keys. pg_cert::load reads both files before any tunnel or proxy starts, so a missing file, a non-PEM file or a locked key is named at once, and passes them to PgConnectOptions as PEM bytes (ssl_client_cert_from_pem), so pooled connections never re-read the files. An encrypted PKCS#8 key (BEGIN ENCRYPTED PRIVATE KEY) is decrypted in memory with the pkcs8 crate (0.11, encryption feature), which russh already builds, so Cargo.lock gained no crate. Decryption runs in spawn_blocking because PBKDF2 or scrypt is slow on purpose. The cert paths live in the stored URL as sslcert and sslkey beside sslrootcert; the passphrase is the Keychain secret pg-key-passphrase, read only when the URL has an sslkey so UI tests never touch the Keychain. Ruled out: the traditional OpenSSL encryption (Proc-Type: 4,ENCRYPTED) and DES ciphers; no maintained crate in the lock opens them, so they are refused with the openssl pkcs8 -topk8 command. Watch out: a key that does not match the certificate surfaces as rustls 'keys may not be consistent: KeyMismatch' at handshake, and a server that wants a certificate answers 28000 'connection requires a valid client certificate', which classify must see before SignIn. Test reads pg_stat_ssl.client_dn in the connect probe; behind a pooler it describes the pooler, so it is dropped there.

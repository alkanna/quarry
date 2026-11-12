---
title: "PostgreSQL role passwords go out as a SCRAM verifier, and role changes are checked before and after in one transaction"
author: Khiman
date: 2026-11-12
branch: feat/pg-roles
cost: { minutes: 45, tokens: 123914 }
id: l-a40a1eee
scope: repo
status: active
src: agent
---

Decided: roles_pg.rs never sends a password in clear. scram_verifier computes the SCRAM-SHA-256 verifier the way PQencryptPasswordConn does (SASLprep for non-ASCII, scram_iterations read from the server, 16 byte salt), checked against a verifier PostgreSQL 18 stored for 'pencil'. Every batch (run_batch) re-reads each change's state in the write transaction and compares it with what the window showed, runs, then reads again and compares with what the change meant to leave; the second check is what catches a REVOKE the server skips because another grantor made the grant (it only warns). Ruled out: comparing relacl text before and after an undo, since a grant then revoke turns a NULL ACL into the explicit owner default, which grants the same; states are read through COALESCE(acl, acldefault(..)). Watch out: aclexplode on an empty aclitem[] raises 'ACL arrays must be one-dimensional', so a missing default ACL row must be NULL, not '{}'. sqlx signs in without SASLprep (sasl.rs hi() uses the raw password), so live tests that sign in with a password set here must use an already NFKC-normalized one. On PostgreSQL 16+ a GRANT on an existing membership updates the INHERIT, SET and ADMIN options it names, TRUE or FALSE.

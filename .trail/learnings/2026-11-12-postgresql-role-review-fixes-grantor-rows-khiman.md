---
title: "PostgreSQL role review fixes: grantor rows, SASLprep, md5 and shared drops"
author: Khiman
date: 2026-11-12
branch: feat/pg-roles
cost: { minutes: 115, tokens: 325389 }
id: l-89b5992e
scope: repo
status: active
src: agent
touches:
  - src/db/sql/roles_pg.rs
  - src/app/pg_roles.rs
---

Decided: roles_pg run_batch records each change's state grantor by grantor (Exact: aclexplode grantor, pg_auth_members.grantor keyed '' before 16 because a pre-16 GRANT WITH ADMIN OPTION may rewrite the one row's grantor) and the undo restores exactly that: for an ACL it diffs the one grantor row that changed and issues plain GRANT/REVOKE as the same login (object GRANTED BY must name the current user, so it cannot reach another grantor), for a membership it names the row with GRANTED BY from 16, which a superuser may do for any grantor (checked on 18.6). Merged comparison alone passed while a second grantor's row stayed behind. On a server whose password_encryption is md5 (the default of the postgres:13 image) Quarry sends the MD5 hash as psql's \password does; refusing would have blocked every PG13 out of the box. Watch out: an MD5-stored password cannot sign in through a scram-sha-256 pg_hba line (images 14+), so live tests sign in with it only on 13. PostgreSQL's pg_saslprep checks prohibited, unassigned (Unicode 3.2) and bidi on the mapped text before NFKC, and a password that maps to nothing is used as typed; the stringprep crate's saslprep checks after NFKC and returns an empty string there, so it is not used. A table REVOKE also revokes the same privilege, and its grant option, on every column (verified), so a REVOKE with column grants of that privilege is refused. to_char of -infinity is NULL, so UNTIL names both infinities. Database CONNECT, tablespace and parameter grants sit in pg_shdepend with dbid 0 and deptype a; a join on pg_database drops them, and one DROP OWNED in any database revokes them.

---
title: "PostgreSQL dump of partitions, row security, grants and owners"
author: Khiman
date: 2026-11-12
branch: feat/pg-dump3
cost: { minutes: 365, tokens: 1029340 }
id: l-56c24be3
scope: repo
status: active
src: agent
touches:
  - "src/db/sql/pg_dump.rs#dump_on"
  - "src/db/sql/pg_dump_tests.rs#lines"
---

decided: grants and owners are options (off by default) written at the very end, owners before grants, because a superuser's GRANT on an object is recorded as the owner's, so the restored ACL text (grantor included) matches the source; grants are acl minus acldefault(kind, owner) via aclexplode with EXCEPT, grantor left out. Partitions are written PARTITION OF their parent and rows read from the leaves only; the parent's keys and indexes use ONLY and each partition's copy is created on the partition then ALTER INDEX .. ATTACH PARTITION before the foreign keys. ruled out: ALTER TABLE ONLY on a partitioned table for a CHECK, a FOREIGN KEY or a NOT NULL (the server refuses: constraint must be added to child tables too / cannot use ONLY for foreign key), and a primary key on a parent sub-partitioned by a column outside the key (the server refuses a unique constraint without every partitioning column). watch out: partition copies of keys have conparentid set and conislocal false, PG18 inherited NOT NULLs carry the parent's name with conislocal false, cloned triggers have tgparentid set; GRANT ON DATABASE needs a name, so it is a DO block over current_database(), which a pooler refuses. In pg_dump_tests the lines() helper leaves SET search_path = '' on a pooled connection, so a later sql() on the same pool must schema-qualify names or it fails with relation does not exist.

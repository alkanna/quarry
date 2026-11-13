---
title: Never pass database passwords on a command line
author: Khiman
date: 2026-11-07
standing: true
id: l-914eb06e
scope: repo
status: active
src: init-review
---

## Watch out
- Dump and restore put the full connection address on the command line, so any local user could read the password with ps.
- In the code: For mongodump and mongorestore, write the URI to a temp config file (mode 0600) and pass --config. Delete the file when the run ends or is stopped.

_From PR #62 here, Nov 6._

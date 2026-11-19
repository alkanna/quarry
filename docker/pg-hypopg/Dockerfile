# PostgreSQL 18 with HypoPG, for the Explain dialog's hypothetical index trial. The official image already lists the
# PGDG apt source, which carries the extension built for this major version.
FROM postgres:18
RUN apt-get update \
 && apt-get install -y --no-install-recommends postgresql-18-hypopg \
 && rm -rf /var/lib/apt/lists/*

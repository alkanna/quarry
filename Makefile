# Common runs of Quarry. `make` alone lists them.
# The live tests use throwaway Docker servers named quarry-test-*, on ports away from the defaults so they never meet a
# real local database. Their password is a fixed test value, as in CI.

VIEW ?= content
OUT ?= shot.png
THEME ?= DARK

PG18 = postgres://postgres:quarry@127.0.0.1:55418/postgres
PG13 = postgres://postgres:quarry@127.0.0.1:55413/postgres
PGBOUNCER = postgres://postgres:quarry@127.0.0.1:56432/postgres
MYSQL = mysql://root:quarry@127.0.0.1:53306
MARIADB = mariadb://root:quarry@127.0.0.1:53307
MONGO = mongodb://localhost:57017/?directConnection=true
LIVE = QUARRY_TEST_POSTGRES='$(PG18)' QUARRY_TEST_POSTGRES13='$(PG13)' QUARRY_TEST_PGBOUNCER='$(PGBOUNCER)' QUARRY_TEST_MYSQL='$(MYSQL)' QUARRY_TEST_MARIADB='$(MARIADB)' QUARRY_TEST_MONGO='$(MONGO)' QUARRY_TEST_MONGO_RS='$(MONGO)'

# Starts a stopped container again, or creates it. One left in "created" never started (its port was taken, say) and
# would come up without its port, so it is made again.
UP = { [ "$$(docker inspect -f '{{.State.Status}}' $(1) 2>/dev/null)" != created ] || docker rm -f $(1) >/dev/null; } && { docker start $(1) >/dev/null 2>&1 || docker run -d --name $(1) $(2) >/dev/null; }

help:
	@echo "Run"
	@echo "  make run                     the app (release)"
	@echo "  make demo                    the offline Mongo demo, to click around in"
	@echo "  make demo-sql VIEW=content   the offline MySQL demo on one view (content, query, shell, structure, ...)"
	@echo "  make demo-pg VIEW=content    the offline PostgreSQL demo on one view (also schemas, connform, paste, tls)"
	@echo "  make screenshot OUT=shot.png THEME=DARK|LIGHT [SWITCH='QUARRY_TEST_PG=content']"
	@echo "Test"
	@echo "  make check                   what CI runs on macOS and Windows"
	@echo "  make test                    unit tests; live tests skip themselves"
	@echo "  make test-live               every test against the Docker servers (starts them)"
	@echo "  make test-ui                 the headless UI tests against the Docker servers"
	@echo "  make test-one T=<name>       tests matching a name, against the Docker servers"
	@echo "  make servers-up | servers-down | servers-status"
	@echo "Build"
	@echo "  make build                   release binary"
	@echo "  make bundle [PLATFORM=...]   package into dist/ (./bundle.sh --help)"
	@echo "  make bench [ARGS=--quick]    memory and frame time (opens windows)"
	@echo "  make release-mac             signed and notarized macOS build (maintainer only)"

run:
	cargo run --release

build:
	cargo build --release

check:
	cargo check --all-targets

test:
	cargo test

test-live: servers-up
	$(LIVE) cargo test

test-ui: servers-up
	$(LIVE) cargo test ui_tests

test-one: servers-up
	$(LIVE) cargo test $(T)

demo:
	QUARRY_DEMO=1 cargo run --release

demo-sql:
	QUARRY_DEMO=1 QUARRY_TEST_SQL=$(VIEW) cargo run --release

demo-pg:
	QUARRY_DEMO=1 QUARRY_TEST_PG=$(VIEW) cargo run --release

# A throwaway HOME, so a screenshot never reads or writes the real settings.
screenshot: build
	HOME=$$(mktemp -d) QUARRY_NO_FOCUS=1 QUARRY_TEST_$(THEME)=1 $(SWITCH) ./target/release/quarry --screenshot $(OUT)

bench:
	python3 bench.py $(ARGS)

bundle:
	./bundle.sh $(PLATFORM)

servers-up:
	@$(call UP,quarry-test-pg18,-p 55418:5432 -e POSTGRES_PASSWORD=quarry postgres:18)
	@$(call UP,quarry-test-pg13,-p 55413:5432 -e POSTGRES_PASSWORD=quarry postgres:13)
	@$(call UP,quarry-test-mysql,-p 53306:3306 -e MYSQL_ROOT_PASSWORD=quarry mysql:8.4)
	@$(call UP,quarry-test-mariadb,-p 53307:3306 -e MARIADB_ROOT_PASSWORD=quarry mariadb:11.4)
	@$(call UP,quarry-test-mongo,-p 57017:27017 mongo:8 --replSet rs0)
	@docker network create quarry-test-pgb >/dev/null 2>&1 || true
	@$(call UP,quarry-test-pgb-pg,--network quarry-test-pgb -e POSTGRES_PASSWORD=quarry postgres:18)
	@$(call UP,quarry-test-pgbouncer,--network quarry-test-pgb -p 56432:5432 -e DB_HOST=quarry-test-pgb-pg -e DB_USER=postgres -e DB_PASSWORD=quarry -e AUTH_TYPE=scram-sha-256 -e POOL_MODE=transaction -e MAX_PREPARED_STATEMENTS=0 -e IGNORE_STARTUP_PARAMETERS=extra_float_digits -e DEFAULT_POOL_SIZE=8 -e MAX_CLIENT_CONN=200 edoburu/pgbouncer)
	@echo "waiting for the servers"
	@until docker exec quarry-test-mongo mongosh --quiet --eval 'try { rs.status() } catch (e) { rs.initiate({ _id: "rs0", members: [{ _id: 0, host: "localhost:27017" }] }) }; quit(db.hello().isWritablePrimary ? 0 : 1)' >/dev/null 2>&1; do sleep 1; done
	@until docker exec quarry-test-mysql mysqladmin -uroot -pquarry ping --silent >/dev/null 2>&1; do sleep 1; done
	@until docker exec quarry-test-mariadb mariadb-admin -uroot -pquarry ping --silent >/dev/null 2>&1; do sleep 1; done
	@until docker exec quarry-test-pg18 pg_isready -U postgres -q && docker exec quarry-test-pg13 pg_isready -U postgres -q && docker exec quarry-test-pgb-pg pg_isready -U postgres -q; do sleep 1; done
	@until docker run --rm --network quarry-test-pgb -e PGPASSWORD=quarry postgres:18 psql -h quarry-test-pgbouncer -U postgres -At -c 'select 1' >/dev/null 2>&1; do sleep 1; done
	@echo "servers ready"

# -v also removes their anonymous data volumes, which would otherwise pile up unused.
servers-down:
	-docker rm -f -v quarry-test-pg18 quarry-test-pg13 quarry-test-mysql quarry-test-mariadb quarry-test-mongo quarry-test-pgbouncer quarry-test-pgb-pg
	-docker network rm quarry-test-pgb

servers-status:
	@docker ps -a --filter name=quarry-test- --format '{{.Names}}\t{{.Status}}\t{{.Ports}}'

# Developer ID build for the maintainer's Mac: needs the cert in the keychain and the "quarry" notarytool profile.
release-mac:
	QUARRY_SIGN_ID=167E682A9DE81BC21DED6018673F23E6EA9DB831 QUARRY_NOTARY_ARGS="--keychain-profile quarry" ./bundle.sh

.PHONY: help run build check test test-live test-ui test-one demo demo-sql demo-pg screenshot bench bundle servers-up servers-down servers-status release-mac

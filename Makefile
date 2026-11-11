# Common runs of Quarry. `make` alone lists them.
# The live tests use throwaway Docker servers named quarry-test-*, on ports away from the defaults so they never meet a
# real local database. Their password is a fixed test value, as in CI.

# Settings for this machine only, such as REMOTE = <ssh alias>. The file is gitignored.
-include local.mk

VIEW ?= content
OUT ?= shot.png
THEME ?= DARK

# SET=n (1 to 9) runs a separate set of servers, named quarry-test-n-* on ports moved by n*100, so several worktrees
# can run the live tests at once: the tests use fixed database names that collide on one shared set.
SET ?= 0
# HOST=<ssh alias> runs the servers on another machine through Docker over SSH, and the tests reach them at its address,
# which takes the database load off this machine. The alias must log in without a password and reach Docker.
HOST ?=
DOCKER = $(if $(HOST),DOCKER_HOST=ssh://$(HOST) )docker
ADDR := $(if $(HOST),$(shell ssh -G $(HOST) 2>/dev/null | awk '/^hostname /{print $$2}'),127.0.0.1)
N = quarry-test$(if $(filter 0,$(SET)),,-$(SET))
P = $(shell expr $(1) + $(SET) \* 100)
PG18 = postgres://postgres:quarry@$(ADDR):$(call P,55418)/postgres
PG13 = postgres://postgres:quarry@$(ADDR):$(call P,55413)/postgres
PGBOUNCER = postgres://postgres:quarry@$(ADDR):$(call P,56432)/postgres
MYSQL = mysql://root:quarry@$(ADDR):$(call P,53306)
MARIADB = mariadb://root:quarry@$(ADDR):$(call P,53307)
MONGO = mongodb://$(ADDR):$(call P,57017)/?directConnection=true
LIVE = QUARRY_TEST_POSTGRES='$(PG18)' QUARRY_TEST_POSTGRES13='$(PG13)' QUARRY_TEST_PGBOUNCER='$(PGBOUNCER)' QUARRY_TEST_MYSQL='$(MYSQL)' QUARRY_TEST_MARIADB='$(MARIADB)' QUARRY_TEST_MONGO='$(MONGO)' QUARRY_TEST_MONGO_RS='$(MONGO)'

# Starts a stopped container again, or creates it. One left in "created" never started (its port was taken, say) and
# would come up without its port, so it is made again.
UP = { [ "$$($(DOCKER) inspect -f '{{.State.Status}}' $(1) 2>/dev/null)" != created ] || $(DOCKER) rm -f $(1) >/dev/null; } && { $(DOCKER) start $(1) >/dev/null 2>&1 || $(DOCKER) run -d --name $(1) $(2) >/dev/null; }

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
	@echo "  make remote-test [SET=n]     every test on REMOTE=<ssh alias> against its own servers, on the committed HEAD"
	@echo "  make coverage                every test against the Docker servers under line coverage, HTML report opened"
	@echo "  make servers-up | servers-down | servers-status"
	@echo "  SET=n on any test or servers target uses its own server set (ports + n*100), for parallel worktrees"
	@echo "  HOST=<ssh alias> runs the servers on that machine through Docker over SSH"
	@echo "  Defaults for this machine (HOST, REMOTE, SET) can go in local.mk, which git ignores"
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

# Runs every test on another machine (REMOTE, an ssh alias) against servers there, on the committed HEAD. Each SET gets its
# own checkout there, ~/quarry-wt/<SET>, which keeps its build between runs. Needs Rust, Bun, Docker and the build
# libraries on that machine, and a repo at ~/quarry that accepts pushes.
REMOTE ?=
remote-test:
	@[ -n "$(REMOTE)" ] || { echo "Set REMOTE=<ssh alias>, here or in local.mk"; exit 1; }
	git push -qf $(REMOTE):quarry HEAD:refs/heads/remote-$(SET)
	ssh $(REMOTE) 'set -e; cd ~/quarry; [ -d ~/quarry-wt/$(SET) ] || git worktree add -qf ~/quarry-wt/$(SET) remote-$(SET); cd ~/quarry-wt/$(SET); git checkout -qf remote-$(SET); git reset -q --hard remote-$(SET); export PATH=$$HOME/.cargo/bin:$$HOME/.bun/bin:$$PATH; make test-live SET=$(SET) $(if $(T),T=$(T))'

# Needs cargo-llvm-cov (cargo install cargo-llvm-cov) and rustup's llvm-tools-preview component.
coverage: servers-up
	$(LIVE) cargo llvm-cov --no-report
	cargo llvm-cov report
	cargo llvm-cov report --open

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
	@$(call UP,$(N)-pg18,-p $(call P,55418):5432 -e POSTGRES_PASSWORD=quarry postgres:18)
	@$(call UP,$(N)-pg13,-p $(call P,55413):5432 -e POSTGRES_PASSWORD=quarry postgres:13)
	@$(call UP,$(N)-mysql,-p $(call P,53306):3306 -e MYSQL_ROOT_PASSWORD=quarry mysql:8.4)
	@$(call UP,$(N)-mariadb,-p $(call P,53307):3306 -e MARIADB_ROOT_PASSWORD=quarry mariadb:11.4)
	@$(call UP,$(N)-mongo,-p $(call P,57017):27017 mongo:8 --replSet rs0)
	@$(DOCKER) network create $(N)-pgb >/dev/null 2>&1 || true
	@$(call UP,$(N)-pgb-pg,--network $(N)-pgb -e POSTGRES_PASSWORD=quarry postgres:18)
	@$(call UP,$(N)-pgbouncer,--network $(N)-pgb -p $(call P,56432):5432 -e DB_HOST=$(N)-pgb-pg -e DB_USER=postgres -e DB_PASSWORD=quarry -e AUTH_TYPE=scram-sha-256 -e POOL_MODE=transaction -e MAX_PREPARED_STATEMENTS=0 -e IGNORE_STARTUP_PARAMETERS=extra_float_digits -e DEFAULT_POOL_SIZE=8 -e MAX_CLIENT_CONN=200 edoburu/pgbouncer)
	@echo "waiting for the servers"
	@until $(DOCKER) exec $(N)-mongo mongosh --quiet --eval 'try { rs.status() } catch (e) { rs.initiate({ _id: "rs0", members: [{ _id: 0, host: "localhost:27017" }] }) }; quit(db.hello().isWritablePrimary ? 0 : 1)' >/dev/null 2>&1; do sleep 1; done
	@until $(DOCKER) exec $(N)-mysql mysqladmin -uroot -pquarry ping --silent >/dev/null 2>&1; do sleep 1; done
	@until $(DOCKER) exec $(N)-mariadb mariadb-admin -uroot -pquarry ping --silent >/dev/null 2>&1; do sleep 1; done
	@until $(DOCKER) exec $(N)-pg18 pg_isready -U postgres -q && $(DOCKER) exec $(N)-pg13 pg_isready -U postgres -q && $(DOCKER) exec $(N)-pgb-pg pg_isready -U postgres -q; do sleep 1; done
	@until $(DOCKER) run --rm --network $(N)-pgb -e PGPASSWORD=quarry postgres:18 psql -h $(N)-pgbouncer -U postgres -At -c 'select 1' >/dev/null 2>&1; do sleep 1; done
	@echo "servers ready"

# -v also removes their anonymous data volumes, which would otherwise pile up unused.
servers-down:
	-$(DOCKER) rm -f -v $(N)-pg18 $(N)-pg13 $(N)-mysql $(N)-mariadb $(N)-mongo $(N)-pgbouncer $(N)-pgb-pg
	-$(DOCKER) network rm $(N)-pgb

servers-status:
	@$(DOCKER) ps -a --filter name=$(N)- --format '{{.Names}}\t{{.Status}}\t{{.Ports}}'

# Developer ID build for the maintainer's Mac: needs the cert in the keychain and the "quarry" notarytool profile.
release-mac:
	QUARRY_SIGN_ID=167E682A9DE81BC21DED6018673F23E6EA9DB831 QUARRY_NOTARY_ARGS="--keychain-profile quarry" ./bundle.sh

.PHONY: help run build check test test-live test-ui test-one remote-test coverage demo demo-sql demo-pg screenshot bench bundle servers-up servers-down servers-status release-mac

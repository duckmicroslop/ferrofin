# Adoption smoke test

Ferrofin adopts a Jellyfin database in place. This harness proves that claim for every
release the gate accepts — **10.11.8, 10.11.9, 10.11.10, 10.11.11, 12.0 and 12.1** (12.1 by
both upgrade routes) — by booting an image on a fresh copy of a real database of each and
checking the result the same way:

1. the boot log names the expected generation, applies every migration and logs no `ERROR`;
2. 38 read-only probes (`smoke.sh`) answer exactly what **Jellyfin 12.1** answers on the same
   library, ignoring only lines that legitimately differ (server version, task and plugin
   lists, `/Devices`, folder order, activity-log count, image byte size);
3. `PRAGMA integrity_check` and `PRAGMA foreign_key_check` are clean on the adopted file;
4. a second boot runs no repair and changes no answer.

It is not a CI gate: it needs a real library, gigabytes of fixtures and about two minutes per
generation. Run it before any change to `crates/ferrofin-db/migrations/`, the adoption gate in
`database.rs`, or the boot repairs in `adoption_repairs.rs`, and before a release.

## The fixtures are yours, not the repository's

Nothing under `adoption/` contains a database. You supply **one** Jellyfin 10.11.8 data
directory and the builder derives the rest with the official Jellyfin images:

```
$FIXTURES/
  jellyfin-10.11.8/            supplied: config/, data/jellyfin.db, root/, metadata/ …
  media-mounts.sh              optional: MEDIA=(-v /host/path:/container/path:ro …)
  jellyfin-10.11.9/            built: one boot of jellyfin/jellyfin:10.11.9 on 10.11.8
  jellyfin-10.11.10/           built: one boot of jellyfin/jellyfin:10.11.10 on 10.11.8
  jellyfin-10.11.11/           built: one boot of jellyfin/jellyfin:10.11.11 on 10.11.8
  jellyfin-12.0/               built: one boot of jellyfin/jellyfin:12.0
  jellyfin-12.1-from-10/       built: one boot of jellyfin/jellyfin:12.1 on 10.11.8
  jellyfin-12.1-from-12/       built: one boot of jellyfin/jellyfin:12.1 on 12.0
  oracle/smoke-jellyfin-12.1.txt  built: Jellyfin 12.1's own smoke answers
  oracle/user.txt              built: the account those answers were probed as
  work/                        scratch; a failed fixture's copy, logs and smoke output stay here
```

Take the snapshot with the server stopped, or copy the database with
`sqlite3 jellyfin.db ".backup /snapshot/data/jellyfin.db"` so the WAL is folded in. Bring
`root/` (library definitions) and `metadata/` (images) along with `data/`; without them the
library list is empty and every poster is a placeholder, which the probes will notice. The
media paths referenced by the library options must resolve inside the container, hence
`media-mounts.sh`. The probes need an administrator with a **Jellyfin Web** session in `Devices`
(the builder records which account the oracle ran as in `oracle/user.txt` and the runner
probes every fixture as that account; `--user NAME` or `ADOPTION_USER` overrides) and an
**API key** in `ApiKeys`. Credentials are read from the copy at run time and never written.

```bash
adoption/build-fixtures.sh --fixtures /path/to/fixtures     # once, ~25 min, pulls 10.11.9–12.1
docker build -t ferrofin:bench .                            # the commit under test
adoption/run.sh --fixtures /path/to/fixtures --image ferrofin:bench
adoption/run.sh --fixtures … --only jellyfin-12.1-from-10   # one generation
```

Output is one line per generation:

```
PASS  10.11.8   jellyfin-10.11.8
PASS  10.11.8   jellyfin-10.11.9
PASS  10.11.11  jellyfin-10.11.10
PASS  10.11.11  jellyfin-10.11.11
PASS  12.0.0    jellyfin-12.0
PASS  12.1.0    jellyfin-12.1-from-10
PASS  12.1.0    jellyfin-12.1-from-12
```

The second column is the generation the gate matched — the id *set*, so 10.11.9 reports
`10.11.8` and 10.11.10 reports `10.11.11`; `run.sh` knows which is expected for which fixture.
A `FAIL` line names every check that failed and points at the diff; the copy is kept under
`work/` with `<name>.server.log`, `<name>.smoke.txt` and `<name>.smoke2.txt` beside it.

## Tests

`adoption/tests/adoption.bats` covers the harness itself without docker or fixtures: the
checks in `lib.sh` (log parsing, smoke normalisation and comparison, the SQLite checks, the
second-boot repair detection, the credential picker) run against canned logs, smoke outputs
and throwaway SQLite files, and the two entry points are exercised for their refusals and the
missing-fixture path. CI runs them with `bats adoption/tests` next to `scripts/tests`; locally
`mise exec bats@latest -- bats adoption/tests` or a system `bats` works.

## Adding a generation

When Jellyfin ships a release with new `__EFMigrationsHistory` ids: add a builder step that
boots that image on the right parent fixture, add a row to `FIXTURE_TABLE` in `run.sh`, refresh
the oracle if that release changes answers (delete `oracle/` and rebuild), then teach the gate
(`JELLYFIN_GENERATIONS` in `crates/ferrofin-db/src/database.rs`). The oracle is always the
newest supported Jellyfin: parity is measured against the current release, not the one the
database came from.

## Why the checks are what they are

- **Generation in the log**, not just "it booted": a database adopted as the wrong generation
  baselines the wrong migrations and runs the wrong one-shot repairs (the 10.11.11 gate once
  skipped the playlist import for exactly that reason).
- **Jellyfin's answers as the oracle**: counts and first items are what a client shows; a
  migration that loses rows or hides them is invisible to a green test suite and obvious here.
- **Second boot**: every repair is keyed in `FerrofinMeta` and must not run twice; answers that
  move between two boots mean a repair is not idempotent.
- **`integrity_check` exempts `IX_Peoples_NameLower`**: it indexes `lower("Name")`, and a host
  `sqlite3` built with ICU lower-cases non-ASCII names differently from the SQLite inside
  Jellyfin and Ferrofin, so it reports rows "missing from index" on a file both servers agree
  with. Nothing else is exempt.

# Upgrading Ferrofin

Operator-facing notes for upgrades that need a manual step or change behaviour in a
way the release notes do not make obvious. Newest first. `CHANGELOG.md` lists *what*
changed; this file says *what you have to do about it*.

Ferrofin's own database upgrades in place: start the new version against the same
data directory and its migrations run on boot. Back up the data directory before a
major-version upgrade.

## Unreleased — Unicode username matching

An automatic, transactional code migration adds or retains `Users.NormalizedUsername`,
populates ICU-based invariant uppercase keys, and enforces a unique index. It runs after
the SQL migrations and before the database is exposed to requests; completion is recorded
as `normalized_usernames_icu_v1` in `FerrofinMeta`. Existing SQL migration checksums are
unchanged. Account IDs, password hashes, permissions, and watch history are preserved.

Login, creation, and renaming now agree for non-ASCII case variants such as `münchen`
and `MÜNCHEN`. If old accounts normalize to the same key, startup refuses with their IDs
and names. Restore/use your pre-upgrade installation to rename the conflicting accounts,
then retry the upgrade; do not merge accounts. Back up the full data directory first.

For Jellyfin adoption, follow the [complete migration procedure](INSTALL.md#migrate-an-existing-jellyfin-installation),
including the separately stored configuration and copying before first startup.

## 1.0.0 — first public release

No manual steps between Ferrofin releases. The baseline for this file starts here;
pre-1.0 development builds were never published and are not an upgrade path.

**Coming from Jellyfin** is a different matter and is covered in the README under
[Migrating from Jellyfin](../README.md#migrating-from-jellyfin): adoption is one-way,
Ferrofin writes `jellyfin.db.pre-ferrofin` before touching anything, and you should back
up the whole Jellyfin data directory yourself first.

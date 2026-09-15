# Upgrading Ferrofin

Operator-facing notes for upgrades that need a manual step or change behaviour in a
way the release notes do not make obvious. Newest first. `CHANGELOG.md` lists *what*
changed; this file says *what you have to do about it*.

Ferrofin's own database upgrades in place: start the new version against the same
data directory and its migrations run on boot. Back up the data directory before a
major-version upgrade.

## Unreleased — the schema moves to Jellyfin 12.0

Back up the data directory before starting this version. On first boot migration `0030`
rebuilds `BaseItems`, `Users`, `Permissions`, `Preferences` and `MediaStreamInfos` into
Jellyfin 12.0's shape (the file is snapshotted to `jellyfin.db.pre-0030` first, next to
the existing `jellyfin.db.pre-0007`), `0031` rebuilds one index and drops `sqlite_stat1`,
and `0032` folds Ferrofin's playlist/collection cache table into Jellyfin's
`LinkedChildren`. Expect one longer start proportional to library size; a second boot is
a no-op. Every file-backed boot now runs `PRAGMA foreign_key_check` (about 0.14 s on a
42k-item library) and refuses to open a database that fails it.

Behaviour that changed with the shape:

- A playlist may now hold the same item more than once (Jellyfin 12 semantics); removing
  an entry removes every occurrence of that item.
- Two users whose names differ only by case can no longer coexist (`NormalizedUsername`
  is unique). Creating or renaming into a case-variant returns the error Jellyfin returns.
- `CleanName`/`CleanValue` use Jellyfin 12's punctuation-stripping form; a forced sort
  name goes through the full sort-name pipeline. Both are recomputed once on first boot.
- Localized user views (e.g. a Live TV view created under a translated name) are
  consolidated onto their name-independent id once, with channels, ancestors and
  display preferences moved along.
- **Adopting a Jellyfin database** now accepts 12.0.0 as well as 10.11.8 (exact
  migration sets, still one-way). A 12.0 database keeps its `LinkedChildren` rows and
  is never re-imported from the frozen JSON copy in `Data`.

## 1.0.0 — first public release

No manual steps between Ferrofin releases. The baseline for this file starts here;
pre-1.0 development builds were never published and are not an upgrade path.

**Coming from Jellyfin** is a different matter and is covered in the README under
[Migrating from Jellyfin](../README.md#migrating-from-jellyfin): adoption is one-way,
Ferrofin writes `jellyfin.db.pre-ferrofin` before touching anything, and you should back
up the whole Jellyfin data directory yourself first.

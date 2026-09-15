# Unicode casing audit and fixes

This follow-up covers the remaining findings from the username PR's targeted
casing audit. It does not claim that every case conversion in the repository has
been reviewed. Upstream comparisons used the local Jellyfin 10.11.8 source.

## Implemented

| Area | Problem | Resolution |
| --- | --- | --- |
| Artist links | SQLite `LOWER(Name)` was ASCII-only, while Rust lowercased the parameter as Unicode. Even an exact `Élodie` request could miss. | Both the query and result grouping now use the shared invariant uppercase mapping. |
| Search and name filters | SQL and Rust disagreed on casing in raw-name, original-title, prefix, and range filters. | Explicit ICU-backed SQL functions and matching Rust mappings replace the mixed conversions. Original-title filters keep accents in their parameters. Search scoring also uses invariant casing. |
| Stored lookup keys | Rust's contextual lowercase made `ΟΣ` into `ος`; .NET invariant lowercase produces `οσ`. Stored and requested clean keys could disagree. | New writes use invariant lowercase. Migration 0031 repairs matching old `CleanName` and `CleanValue` keys together. Existing plain-column indexes remain usable. |
| Derived sort keys | Default and forced sort-key derivation used full lowercase. Fixing only query parameters would leave stale stored keys. | New derivations use invariant lowercase. Migration 0031 also repairs sort keys that match the previous derivation, preserving custom keys and the original `ForcedSortName`. |
| By-name identity | Rust expands `İ` when lowercasing; .NET invariant casing leaves it unchanged, producing a different MD5 item ID. | Jellyfin-mode IDs now use invariant lowercase. Existing person IDs are reused through the indexed clean-name lookup and a previous-ID fallback. The identity-unification pass recognizes previous IDs too. Years under affected Unicode metadata roots retain existing rows. Legacy ID mode stays unchanged. |
| Person DTOs | Grouping with full lowercase could conflate names whose database clean keys are distinct, or fail to group equal clean keys. | Grouping now uses precisely the clean key used by the library resolver. Tests verify both shared IDs and distinct IDs. |

The earlier scanner aggregation and music-genre deduplication fixes remain on
this branch, in commit `08a630a`.

### Two corrections to the original audit

- Jellyfin 10.11.8's `BaseItemRepository.FindArtists` actually uses exact names.
  Ferrofin already had tested case-insensitive behavior. This change retains
  that behavior and makes it Unicode-aware; it is not an exact port of that
  upstream method.
- Person DTO grouping must follow **database clean keys**, not automatically
  use ordinal-ignore-case uppercase. For example, `ΟΣ` and `οσ` share a clean
  key, whereas `ΟΣ` and `ος` do not. Using the username comparer here would
  conflate identities that the database distinguishes.

## Migration and identity guarantees

- Migrations 0001–0030 are unchanged. 0031 is a new transactional data migration;
  it adds no tables, columns, indexes, triggers, or views.
- Updates require the stored value to match the previous Ferrofin derivation.
  Keys from other implementations and custom keys are not blindly overwritten.
- Raw names, forced sort-title text, item IDs, item-value IDs, and references are
  retained. Clean-key collisions do not delete or merge existing items.
- Existing person IDs remain valid on refresh, including a refresh that changes
  the name's casing. Already-existing duplicate identities are not globally
  consolidated by this change.
- Apply 0031 through Ferrofin startup. Standalone `sqlite3` or `sqlx migrate`
  does not register the application functions used by this data migration.
  Ordinary external database inspection remains possible: no persistent schema
  objects depend on these functions.

The separately developed Jellyfin 12 database work must follow migration 0031.
It must also retain fail-fast normalization errors and collision checks before
creating a unique normalized-username index.

## SQL function safety

`sqlite_casing.rs` registers explicitly named functions on all application
connections, including migration, reader, and writer connections. SQLite's
built-in `lower`, `upper`, `LIKE`, and `NOCASE` retain their original meanings.
The custom functions are deterministic and `DIRECTONLY`, preventing persistent
schema expressions from depending on them. They preserve NULL and embedded NUL,
reject invalid UTF-8, and catch Rust unwinding at the C callback boundary.

## Validation and cost

Regression coverage includes Greek sigma/contextual casing, Greek extended
letters, accents, Turkish I, sharp s, supplementary-plane letters, NULL,
embedded NUL, migration retry, custom-key preservation, retained favorites,
changed credit spelling, and person DTO identity separation. The existing
full-Unicode .NET casing oracle continues to validate the shared helpers.

The ignored `unicode_query_cost` test is a reproducible microbenchmark of the
artist query's scan: 20,000 names, 90% ASCII, median of 21 iterations. On this
machine's **unoptimized test build**, the previous SQLite ASCII query took
4.86 ms and the ICU query took 11.24 ms. These are query-cost measurements, not
production HTTP throughput. An ASCII fast path avoids ICU work for ASCII text.
Read-time Unicode casing adds CPU cost; clean-name equality joins retain their
existing indexed stored keys. Larger-library performance may justify separately
indexed Unicode keys in a future change.

Reproduce with:

```sh
cargo test -p ferrofin-db --test invariant_clean_names unicode_query_cost -- --ignored --nocapture
```

Locale-sensitive collation, the existing sort-name transliteration/digit-padding
limitations, and the existing diacritic-removal approximations are separate from
this casing correction. This change does not introduce Unicode normalization or
confusable-character matching for usernames.

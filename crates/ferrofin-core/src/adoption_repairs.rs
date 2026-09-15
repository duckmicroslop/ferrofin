//! One-shot boot repairs that port Jellyfin 12.0's code migrations for the
//! data a schema migration cannot compute — the routines under
//! `Jellyfin.Server/Migrations/Routines/2026*` that touch linked children,
//! extras and version groups.
//!
//! Each repair is keyed in `FerrofinMeta` (or on the adoption record) and runs
//! once per database; a second boot is a no-op. They run on every path — a
//! fresh database, an upgraded install, an adopted 10.11.8 or 12.0 database —
//! and are written so a database that is already in the 12.0 state is left
//! alone (an adopted 12.0 database went through the real routines in Jellyfin).

use std::path::Path;

use ferrofin_db::Database;
use ferrofin_db::store::guid_to_db;
use ferrofin_model::data::BaseItemKind;
use ferrofin_traits::error::ServiceError;
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::db_error::db_err;
use crate::item_data::{import_membership, parse_data, read_linked_children};
use crate::item_persistence_service::alternate_version_child_type;
use crate::item_type_lookup::stored_type_name;

/// `LinkedChildType::LocalAlternateVersion`.
const LOCAL_ALTERNATE_VERSION: i64 = 2;
/// `LinkedChildType::LinkedAlternateVersion`.
const LINKED_ALTERNATE_VERSION: i64 = 3;
/// The placeholder item 12.0's `AddForeignKeyToOwnerId` re-points dangling
/// owners at, and `CleanupOrphanedExtras` then deletes the owned rows of.
const PLACEHOLDER_ID: &str = "00000000-0000-0000-0000-000000000001";

/// Runs every repair in this module, in upstream's order.
///
/// # Errors
/// Returns [`ServiceError`] if a repair's queries fail.
pub async fn run_all(db: &Database) -> Result<(), ServiceError> {
    let imported = import_membership_once(db).await?;
    if imported > 0 {
        tracing::info!(
            rows = imported,
            "imported playlist/collection/version membership from Data JSON"
        );
    }
    let removed = cleanup_orphaned_extras(db).await?;
    if removed > 0 {
        tracing::info!(
            items = removed,
            "removed extras whose owner no longer exists"
        );
    }
    let fixed = fix_owner_id_relationships(db).await?;
    if fixed > 0 {
        tracing::info!(items = fixed, "repaired OwnerId relationships");
    }
    let linked = backfill_alternate_version_links(db).await?;
    if linked > 0 {
        tracing::info!(
            rows = linked,
            "backfilled alternate-version links from PrimaryVersionId"
        );
    }
    Ok(())
}

/// A `ferrofin-db` error on the bookkeeping tables, as a service error.
fn meta_err(err: impl std::fmt::Display) -> ServiceError {
    ServiceError::Backend(err.to_string())
}

/// Whether `key` still has to run.
async fn once(db: &Database, key: &str) -> Result<bool, ServiceError> {
    if db.meta_get(key).await.map_err(meta_err)?.is_some() {
        return Ok(false);
    }
    Ok(true)
}

async fn done(db: &Database, key: &str) -> Result<(), ServiceError> {
    db.meta_set(key, "1").await.map_err(meta_err)
}

/// The one-shot port of `MigrateLinkedChildren`'s data move for a database
/// **newly adopted from 10.11.8** — the only case where the `Data` JSON is the
/// truth about membership. It reads `LinkedChildren` from every playlist/box
/// set and `LocalAlternateVersions` / `LinkedAlternateVersions` from every
/// video, writes the rows, and flips the adoption record's flag in the same
/// transaction. A 12.0 adoption, a Ferrofin-native database and an install
/// adopted before the record existed have no such record and are skipped.
///
/// Returns the number of rows written.
///
/// # Errors
/// Returns [`ServiceError`] if the underlying queries fail.
pub async fn import_membership_once(db: &Database) -> Result<usize, ServiceError> {
    let Some(state) = db.adoption_state().await.map_err(meta_err)? else {
        return Ok(0);
    };
    if state.generation != "10.11.8" || state.membership_import_done {
        return Ok(0);
    }
    let playlist = stored_type_name(BaseItemKind::Playlist).unwrap_or_default();
    let boxset = stored_type_name(BaseItemKind::BoxSet).unwrap_or_default();
    let video_types = [
        stored_type_name(BaseItemKind::Video).unwrap_or_default(),
        stored_type_name(BaseItemKind::Movie).unwrap_or_default(),
        stored_type_name(BaseItemKind::Episode).unwrap_or_default(),
    ];
    let rows: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        r#"SELECT "Id", "Type", "Path", "Data" FROM "BaseItems"
           WHERE "Data" IS NOT NULL AND "Type" IN (?1, ?2, ?3, ?4, ?5)"#,
    )
    .bind(playlist)
    .bind(boxset)
    .bind(video_types[0])
    .bind(video_types[1])
    .bind(video_types[2])
    .fetch_all(db.pool())
    .await
    .map_err(db_err)?;

    let mut tx = db.writer().begin().await.map_err(db_err)?;
    let mut written = 0usize;
    for (id, type_, path, data) in rows {
        let map = parse_data(data.as_deref());
        if type_ == playlist || type_ == boxset {
            if map.contains_key("LinkedChildren") {
                written += import_membership(&mut tx, &id, &map).await?;
            }
        } else {
            written += import_video_alternate_versions(&mut tx, &id, path.as_deref(), &map).await?;
        }
    }
    sqlx::query(r#"UPDATE "FerrofinAdoption" SET "MembershipImportDone" = 1"#)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
    tx.commit().await.map_err(db_err)?;
    Ok(written)
}

/// `MigrateLinkedChildren.ProcessVideoAlternateVersions`: `LocalAlternateVersions`
/// (path strings → `ChildType 2`) then `LinkedAlternateVersions` (linked-child
/// objects → `ChildType 3`), one shared ordinal counter; a pair already
/// present is left alone (local beats linked), a linked version pointing at an
/// item the parent owns is skipped.
async fn import_video_alternate_versions(
    tx: &mut sqlx::SqliteConnection,
    parent_db: &str,
    _parent_path: Option<&str>,
    map: &Map<String, Value>,
) -> Result<usize, ServiceError> {
    let mut candidates: Vec<(String, i64)> = Vec::new();
    if let Some(paths) = map.get("LocalAlternateVersions").and_then(Value::as_array) {
        for path in paths.iter().filter_map(Value::as_str) {
            if path.is_empty() {
                continue;
            }
            if let Some(child) = id_by_path(tx, path).await? {
                candidates.push((child, LOCAL_ALTERNATE_VERSION));
            } else {
                tracing::warn!(
                    path,
                    parent = parent_db,
                    "could not resolve LocalAlternateVersion path"
                );
            }
        }
    }
    let mut linked_map = Map::new();
    if let Some(v) = map.get("LinkedAlternateVersions") {
        linked_map.insert("LinkedChildren".to_owned(), v.clone());
    }
    for child in read_linked_children(&linked_map) {
        let resolved = match child
            .item_id
            .as_deref()
            .and_then(|i| Uuid::parse_str(i).ok())
        {
            Some(id) => Some(guid_to_db(id)),
            None => match child.path.as_deref() {
                Some(p) => id_by_path(tx, p).await?,
                None => None,
            },
        };
        if let Some(child_db) = resolved {
            candidates.push((child_db, LINKED_ALTERNATE_VERSION));
        } else {
            tracing::warn!(
                parent = parent_db,
                "could not resolve LinkedAlternateVersion child"
            );
        }
    }
    let mut written = 0usize;
    for (child_db, child_type) in candidates {
        if child_type == LINKED_ALTERNATE_VERSION {
            let owned: Option<i64> = sqlx::query_scalar(
                r#"SELECT 1 FROM "BaseItems" WHERE "Id" = ?1 AND "OwnerId" = ?2"#,
            )
            .bind(&child_db)
            .bind(parent_db)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?;
            if owned.is_some() {
                continue;
            }
        }
        let exists: Option<i64> = sqlx::query_scalar(
            r#"SELECT 1 FROM "LinkedChildren" WHERE "ParentId" = ?1 AND "ChildId" = ?2"#,
        )
        .bind(parent_db)
        .bind(&child_db)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        if exists.is_some() {
            continue;
        }
        let child_exists: Option<i64> =
            sqlx::query_scalar(r#"SELECT 1 FROM "BaseItems" WHERE "Id" = ?1"#)
                .bind(&child_db)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        if child_exists.is_none() {
            continue;
        }
        append_link(tx, parent_db, &child_db, child_type).await?;
        written += 1;
    }
    Ok(written)
}

async fn id_by_path(
    tx: &mut sqlx::SqliteConnection,
    path: &str,
) -> Result<Option<String>, ServiceError> {
    sqlx::query_scalar(r#"SELECT "Id" FROM "BaseItems" WHERE "Path" = ?1 ORDER BY "Id" LIMIT 1"#)
        .bind(path)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)
}

async fn append_link(
    tx: &mut sqlx::SqliteConnection,
    parent_db: &str,
    child_db: &str,
    child_type: i64,
) -> Result<(), ServiceError> {
    sqlx::query(
        r#"INSERT INTO "LinkedChildren" ("ParentId", "SortOrder", "ChildId", "ChildType")
           VALUES (?1,
               (SELECT COALESCE(MAX("SortOrder"), -1) + 1 FROM "LinkedChildren" WHERE "ParentId" = ?1),
               ?2, ?3)"#,
    )
    .bind(parent_db)
    .bind(child_db)
    .bind(child_type)
    .execute(&mut *tx)
    .await
    .map_err(db_err)?;
    Ok(())
}

/// `CleanupOrphanedExtras`: delete every item whose `OwnerId` is the
/// placeholder — where 0030 (`AddForeignKeyToOwnerId`) re-pointed owners that
/// no longer exist. Links are cleared first; the rows' children cascade.
///
/// # Errors
/// Returns [`ServiceError`] if the underlying queries fail.
pub async fn cleanup_orphaned_extras(db: &Database) -> Result<usize, ServiceError> {
    const KEY: &str = "cleanup_orphaned_extras_v12";
    if !once(db, KEY).await? {
        return Ok(0);
    }
    let ids: Vec<String> =
        sqlx::query_scalar(r#"SELECT "Id" FROM "BaseItems" WHERE "OwnerId" = ?1 AND "Id" <> ?1"#)
            .bind(PLACEHOLDER_ID)
            .fetch_all(db.pool())
            .await
            .map_err(db_err)?;
    let mut tx = db.writer().begin().await.map_err(db_err)?;
    for id in &ids {
        sqlx::query(r#"DELETE FROM "LinkedChildren" WHERE "ParentId" = ?1 OR "ChildId" = ?1"#)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query(r#"DELETE FROM "BaseItems" WHERE "Id" = ?1"#)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
    }
    tx.commit().await.map_err(db_err)?;
    done(db, KEY).await?;
    Ok(ids.len())
}

/// `FixIncorrectOwnerIdRelationships` steps 1–4. Step 1: items sharing a
/// `Path` are de-duplicated — the keeper has direct children, else owns
/// extras, else is not a plain `Folder`, else is the newest; the rest are
/// deleted. Step 2: a video/movie that is not an extra but has an owner that
/// is itself a video/movie, or no longer exists, loses its `OwnerId`. Step 3:
/// an orphaned extra is re-attached to the first video/movie whose path
/// starts with the extra's directory, else loses its owner. Step 4: every
/// version link's child gets `PrimaryVersionId = ParentId`.
///
/// # Errors
/// Returns [`ServiceError`] if the underlying queries fail.
pub async fn fix_owner_id_relationships(db: &Database) -> Result<usize, ServiceError> {
    const KEY: &str = "owner_id_relationships_v12";
    if !once(db, KEY).await? {
        return Ok(0);
    }
    let video = stored_type_name(BaseItemKind::Video).unwrap_or_default();
    let movie = stored_type_name(BaseItemKind::Movie).unwrap_or_default();
    let folder = stored_type_name(BaseItemKind::Folder).unwrap_or_default();
    let mut tx = db.writer().begin().await.map_err(db_err)?;
    let deleted = dedupe_paths(&mut tx, folder).await?;
    // Step 2: videos/movies (not extras) owned by a video/movie or by nothing.
    let cleared = sqlx::query(
        r#"UPDATE "BaseItems" SET "OwnerId" = NULL
           WHERE "OwnerId" IS NOT NULL
             AND ("ExtraType" IS NULL OR "ExtraType" = 0)
             AND "Type" IN (?1, ?2)
             AND (
               "OwnerId" NOT IN (SELECT "Id" FROM "BaseItems")
               OR "OwnerId" IN (SELECT "Id" FROM "BaseItems" WHERE "Type" IN (?1, ?2))
             )"#,
    )
    .bind(video)
    .bind(movie)
    .execute(&mut *tx)
    .await
    .map_err(db_err)?
    .rows_affected();
    // Step 3: orphaned extras re-attached by directory, else unowned.
    let orphans: Vec<(String, Option<String>)> = sqlx::query_as(
        r#"SELECT "Id", "Path" FROM "BaseItems"
           WHERE "ExtraType" IS NOT NULL AND "ExtraType" <> 0 AND "OwnerId" IS NOT NULL
             AND "OwnerId" NOT IN (SELECT "Id" FROM "BaseItems")"#,
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(db_err)?;
    let mut reassigned = 0usize;
    for (id, path) in orphans {
        let Some(path) = path else { continue };
        let Some(dir) = Path::new(&path)
            .parent()
            .map(|d| d.to_string_lossy().into_owned())
        else {
            continue;
        };
        let parent: Option<String> = sqlx::query_scalar(
            r#"SELECT "Id" FROM "BaseItems"
               WHERE "Type" IN (?1, ?2) AND "Path" IS NOT NULL AND "Path" LIKE ?3 || '%'
               ORDER BY "Id" LIMIT 1"#,
        )
        .bind(video)
        .bind(movie)
        .bind(&dir)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?;
        sqlx::query(r#"UPDATE "BaseItems" SET "OwnerId" = ?2 WHERE "Id" = ?1"#)
            .bind(&id)
            .bind(parent)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        reassigned += 1;
    }
    // Step 4: the pointer follows the link.
    let pointed = sqlx::query(
        r#"UPDATE "BaseItems" SET "PrimaryVersionId" = (
               SELECT lc."ParentId" FROM "LinkedChildren" lc
               WHERE lc."ChildId" = "BaseItems"."Id" AND lc."ChildType" IN (2, 3)
               ORDER BY lc."ParentId", lc."SortOrder" LIMIT 1)
           WHERE "Id" IN (SELECT "ChildId" FROM "LinkedChildren" WHERE "ChildType" IN (2, 3))
             AND ("PrimaryVersionId" IS NULL OR "PrimaryVersionId" NOT IN (
               SELECT lc."ParentId" FROM "LinkedChildren" lc
               WHERE lc."ChildId" = "BaseItems"."Id" AND lc."ChildType" IN (2, 3)))"#,
    )
    .execute(&mut *tx)
    .await
    .map_err(db_err)?
    .rows_affected();
    tx.commit().await.map_err(db_err)?;
    done(db, KEY).await?;
    Ok(usize::try_from(cleared + pointed).unwrap_or(usize::MAX) + reassigned + deleted)
}

/// `FixIncorrectOwnerIdRelationships` step 1: items sharing a `Path`.
async fn dedupe_paths(
    tx: &mut sqlx::SqliteConnection,
    folder: &str,
) -> Result<usize, ServiceError> {
    // Step 1: duplicate paths. `(has_children, has_extras, not_folder, created)`
    // sorts the keeper first; the query returns rows newest-first so the
    // last tiebreak is already in order.
    let dups: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        r#"SELECT b."Path", b."Id", b."Type", b."DateCreated" FROM "BaseItems" b
           WHERE b."Path" IS NOT NULL
             AND b."Path" IN (SELECT "Path" FROM "BaseItems" WHERE "Path" IS NOT NULL
                              GROUP BY "Path" HAVING COUNT(*) > 1)
           ORDER BY b."Path", b."DateCreated" DESC, b."Id""#,
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(db_err)?;
    let mut deleted = 0usize;
    let mut by_path: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for (path, id, type_, _) in dups {
        match by_path.last_mut() {
            Some((p, rows)) if *p == path => rows.push((id, type_)),
            _ => by_path.push((path, vec![(id, type_)])),
        }
    }
    for (_, rows) in by_path {
        let mut ranked: Vec<(u8, u8, u8, usize, String)> = Vec::new();
        for (index, (id, type_)) in rows.iter().enumerate() {
            let children: i64 =
                sqlx::query_scalar(r#"SELECT COUNT(*) FROM "BaseItems" WHERE "ParentId" = ?1"#)
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db_err)?;
            let extras: i64 =
                sqlx::query_scalar(r#"SELECT COUNT(*) FROM "BaseItems" WHERE "OwnerId" = ?1"#)
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db_err)?;
            ranked.push((
                u8::from(children > 0),
                u8::from(extras > 0),
                u8::from(*type_ != folder),
                index,
                id.clone(),
            ));
        }
        // Highest flags win; equal flags → lowest index (newest DateCreated).
        ranked.sort_by(|a, b| (b.0, b.1, b.2, a.3).cmp(&(a.0, a.1, a.2, b.3)));
        for (_, _, _, _, id) in ranked.iter().skip(1) {
            sqlx::query(r#"DELETE FROM "LinkedChildren" WHERE "ParentId" = ?1 OR "ChildId" = ?1"#)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            sqlx::query(r#"UPDATE "BaseItems" SET "OwnerId" = NULL WHERE "OwnerId" = ?1"#)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            sqlx::query(r#"DELETE FROM "BaseItems" WHERE "Id" = ?1"#)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

/// Ferrofin modelled a version group by `PrimaryVersionId` alone; 12.0 reads
/// versions from `LinkedChildren`. Write the missing rows for every existing
/// group: local (2) when the two files share a directory, linked (3) otherwise.
/// Rows already present (an adopted 12.0 database) are left alone.
///
/// # Errors
/// Returns [`ServiceError`] if the underlying queries fail.
pub async fn backfill_alternate_version_links(db: &Database) -> Result<usize, ServiceError> {
    const KEY: &str = "alternate_version_links_v12";
    if !once(db, KEY).await? {
        return Ok(0);
    }
    let pairs: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT a."Id", a."PrimaryVersionId" FROM "BaseItems" a
           WHERE a."PrimaryVersionId" IS NOT NULL
             AND a."PrimaryVersionId" IN (SELECT "Id" FROM "BaseItems")
             AND NOT EXISTS (SELECT 1 FROM "LinkedChildren" lc
                             WHERE lc."ParentId" = a."PrimaryVersionId" AND lc."ChildId" = a."Id"
                               AND lc."ChildType" IN (2, 3))
           ORDER BY a."PrimaryVersionId", a."Id""#,
    )
    .fetch_all(db.pool())
    .await
    .map_err(db_err)?;
    let mut tx = db.writer().begin().await.map_err(db_err)?;
    let mut written = 0usize;
    for (child, primary) in &pairs {
        let (Ok(child_id), Ok(primary_id)) = (Uuid::parse_str(child), Uuid::parse_str(primary))
        else {
            continue;
        };
        let child_type = alternate_version_child_type(&mut tx, child_id, primary_id).await?;
        append_link(&mut tx, primary, child, child_type).await?;
        written += 1;
    }
    tx.commit().await.map_err(db_err)?;
    done(db, KEY).await?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use ferrofin_db::store::guid_to_db;
    use ferrofin_model::data::BaseItemKind;
    use uuid::Uuid;

    use super::*;
    use crate::test_support::{seed_item, seed_named_item, test_db};

    async fn set_path(db: &Database, id: Uuid, path: &str) {
        sqlx::query(r#"UPDATE "BaseItems" SET "Path" = ?2 WHERE "Id" = ?1"#)
            .bind(guid_to_db(id))
            .bind(path)
            .execute(db.writer())
            .await
            .expect("path");
    }

    async fn set_data(db: &Database, id: Uuid, data: &str) {
        sqlx::query(r#"UPDATE "BaseItems" SET "Data" = ?2 WHERE "Id" = ?1"#)
            .bind(guid_to_db(id))
            .bind(data)
            .execute(db.writer())
            .await
            .expect("data");
    }

    async fn exists(db: &Database, id: Uuid) -> bool {
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "BaseItems" WHERE "Id" = ?1"#)
            .bind(guid_to_db(id))
            .fetch_one(db.pool())
            .await
            .expect("count")
            > 0
    }

    async fn owner_of(db: &Database, id: Uuid) -> Option<String> {
        sqlx::query_scalar(r#"SELECT "OwnerId" FROM "BaseItems" WHERE "Id" = ?1"#)
            .bind(guid_to_db(id))
            .fetch_one(db.pool())
            .await
            .expect("owner")
    }

    async fn links(db: &Database) -> Vec<(String, String, i64, i64)> {
        sqlx::query_as(
            r#"SELECT "ParentId", "ChildId", "ChildType", "SortOrder" FROM "LinkedChildren"
               ORDER BY "ParentId", "SortOrder""#,
        )
        .fetch_all(db.pool())
        .await
        .expect("links")
    }

    #[tokio::test]
    async fn membership_is_imported_once_for_a_new_10_11_8_adoption() {
        let db = test_db().await;
        db.record_adoption("10.11.8").await.expect("record");
        let (playlist, a, b, primary, alt) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        seed_named_item(&db, playlist, BaseItemKind::Playlist, "P").await;
        for id in [a, b, primary, alt] {
            seed_item(&db, id, BaseItemKind::Movie).await;
        }
        set_path(&db, b, "/m/b.mkv").await;
        set_path(&db, alt, "/m/alt.mkv").await;
        // A 10.11.8 playlist blob: one child by id, one by path, a duplicate.
        set_data(
            &db,
            playlist,
            &format!(
                r#"{{"LinkedChildren":[{{"Type":"Manual","ItemId":"{}"}},{{"Type":"Manual","Path":"/m/b.mkv"}},{{"Type":"Manual","ItemId":"{}"}}]}}"#,
                a.simple(),
                a.simple()
            ),
        )
        .await;
        // A 10.11.8 video blob: a linked alternate version by id and a local one by path.
        set_data(
            &db,
            primary,
            &format!(
                r#"{{"LocalAlternateVersions":["/m/alt.mkv"],"LinkedAlternateVersions":[{{"Type":"Manual","ItemId":"{}"}}]}}"#,
                b.simple()
            ),
        )
        .await;

        let written = import_membership_once(&db).await.expect("import");
        assert_eq!(written, 5);
        assert_eq!(
            links(&db)
                .await
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            vec![
                (guid_to_db(playlist), guid_to_db(a), 0, 0),
                (guid_to_db(playlist), guid_to_db(b), 0, 1),
                (guid_to_db(playlist), guid_to_db(a), 0, 2),
                (guid_to_db(primary), guid_to_db(alt), 2, 0),
                (guid_to_db(primary), guid_to_db(b), 3, 1),
            ]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
        );
        assert!(
            db.adoption_state()
                .await
                .expect("state")
                .expect("record")
                .membership_import_done
        );
        // A second boot changes nothing, even after the table is emptied.
        sqlx::query(r#"DELETE FROM "LinkedChildren""#)
            .execute(db.writer())
            .await
            .expect("empty");
        assert_eq!(import_membership_once(&db).await.expect("again"), 0);
        assert!(links(&db).await.is_empty());
    }

    /// The JSON on a 12.0 database is frozen: an emptied playlist must stay
    /// empty, however much stale JSON it still carries. A Ferrofin-native
    /// database (no record) never imports either.
    #[tokio::test]
    async fn membership_is_never_imported_for_12_0_or_native_databases() {
        for record in [Some("12.0.0"), None] {
            let db = test_db().await;
            if let Some(generation) = record {
                db.record_adoption(generation).await.expect("record");
            }
            let (playlist, a) = (Uuid::new_v4(), Uuid::new_v4());
            seed_named_item(&db, playlist, BaseItemKind::Playlist, "P").await;
            seed_item(&db, a, BaseItemKind::Movie).await;
            set_data(
                &db,
                playlist,
                &format!(
                    r#"{{"LinkedChildren":[{{"Type":"Manual","ItemId":"{}"}}]}}"#,
                    a.simple()
                ),
            )
            .await;
            for _ in 0..2 {
                assert_eq!(import_membership_once(&db).await.expect("import"), 0);
                assert!(links(&db).await.is_empty(), "record {record:?}");
            }
        }
    }

    #[tokio::test]
    async fn orphaned_extras_owned_by_the_placeholder_are_deleted_once() {
        let db = test_db().await;
        let extra = Uuid::new_v4();
        seed_item(&db, extra, BaseItemKind::Trailer).await;
        sqlx::query(r#"UPDATE "BaseItems" SET "OwnerId" = ?2 WHERE "Id" = ?1"#)
            .bind(guid_to_db(extra))
            .bind(PLACEHOLDER_ID)
            .execute(db.writer())
            .await
            .expect("own");
        assert_eq!(cleanup_orphaned_extras(&db).await.expect("cleanup"), 1);
        let left: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "BaseItems" WHERE "Id" = ?1"#)
            .bind(guid_to_db(extra))
            .fetch_one(db.pool())
            .await
            .expect("count");
        assert_eq!(left, 0);
        assert_eq!(cleanup_orphaned_extras(&db).await.expect("again"), 0);
    }

    #[tokio::test]
    async fn owner_id_relationships_are_repaired_like_upstream() {
        let db = test_db().await;
        let (keeper, dup, movie, owned_movie, extra, primary, alt) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        for id in [keeper, dup, movie, owned_movie, primary, alt] {
            seed_item(&db, id, BaseItemKind::Movie).await;
        }
        seed_item(&db, extra, BaseItemKind::Trailer).await;
        // Step 1: two rows share a path; the one with a child is the keeper.
        set_path(&db, keeper, "/m/same.mkv").await;
        set_path(&db, dup, "/m/same.mkv").await;
        sqlx::query(r#"UPDATE "BaseItems" SET "ParentId" = ?2 WHERE "Id" = ?1"#)
            .bind(guid_to_db(extra))
            .bind(guid_to_db(keeper))
            .execute(db.writer())
            .await
            .expect("child");
        // Step 2: a movie owned by another movie is not an extra — unowned.
        sqlx::query(r#"UPDATE "BaseItems" SET "OwnerId" = ?2 WHERE "Id" = ?1"#)
            .bind(guid_to_db(owned_movie))
            .bind(guid_to_db(movie))
            .execute(db.writer())
            .await
            .expect("own");
        // Step 3: an extra whose owner is gone is re-attached by directory.
        set_path(&db, movie, "/m/film/film.mkv").await;
        set_path(&db, extra, "/m/film/trailer.mkv").await;
        let gone = Uuid::new_v4();
        seed_item(&db, gone, BaseItemKind::Movie).await;
        sqlx::query(r#"UPDATE "BaseItems" SET "OwnerId" = ?2, "ExtraType" = 1 WHERE "Id" = ?1"#)
            .bind(guid_to_db(extra))
            .bind(guid_to_db(gone))
            .execute(db.writer())
            .await
            .expect("orphan");
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(db.writer())
            .await
            .expect("fk off");
        sqlx::query(r#"DELETE FROM "BaseItems" WHERE "Id" = ?1"#)
            .bind(guid_to_db(gone))
            .execute(db.writer())
            .await
            .expect("delete owner");
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(db.writer())
            .await
            .expect("fk on");
        // Step 4: a version link whose child has no pointer yet.
        sqlx::query(
            r#"INSERT INTO "LinkedChildren" ("ParentId", "SortOrder", "ChildId", "ChildType")
               VALUES (?1, 0, ?2, 3)"#,
        )
        .bind(guid_to_db(primary))
        .bind(guid_to_db(alt))
        .execute(db.writer())
        .await
        .expect("link");

        let touched = fix_owner_id_relationships(&db).await.expect("repair");
        assert!(touched >= 4, "{touched}");
        assert!(exists(&db, keeper).await && !exists(&db, dup).await);
        assert_eq!(owner_of(&db, owned_movie).await, None);
        assert_eq!(owner_of(&db, extra).await, Some(guid_to_db(movie)));
        let pointer: Option<String> =
            sqlx::query_scalar(r#"SELECT "PrimaryVersionId" FROM "BaseItems" WHERE "Id" = ?1"#)
                .bind(guid_to_db(alt))
                .fetch_one(db.pool())
                .await
                .expect("pointer");
        assert_eq!(pointer, Some(guid_to_db(primary)));
        assert_eq!(fix_owner_id_relationships(&db).await.expect("again"), 0);
    }

    #[tokio::test]
    async fn version_links_are_backfilled_from_primary_version_id_once() {
        let db = test_db().await;
        let (primary, local, linked, already) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        for id in [primary, local, linked, already] {
            seed_item(&db, id, BaseItemKind::Movie).await;
        }
        set_path(&db, primary, "/m/film/film 1080p.mkv").await;
        set_path(&db, local, "/m/film/film 2160p.mkv").await;
        set_path(&db, linked, "/m/other/film.mkv").await;
        for id in [local, linked, already] {
            sqlx::query(r#"UPDATE "BaseItems" SET "PrimaryVersionId" = ?2 WHERE "Id" = ?1"#)
                .bind(guid_to_db(id))
                .bind(guid_to_db(primary))
                .execute(db.writer())
                .await
                .expect("pointer");
        }
        sqlx::query(
            r#"INSERT INTO "LinkedChildren" ("ParentId", "SortOrder", "ChildId", "ChildType")
               VALUES (?1, 0, ?2, 3)"#,
        )
        .bind(guid_to_db(primary))
        .bind(guid_to_db(already))
        .execute(db.writer())
        .await
        .expect("existing");

        assert_eq!(
            backfill_alternate_version_links(&db)
                .await
                .expect("backfill"),
            2
        );
        let mut got: Vec<(String, i64)> = links(&db)
            .await
            .into_iter()
            .map(|(_, c, t, _)| (c, t))
            .collect();
        got.sort();
        let mut want = vec![
            (guid_to_db(already), 3),
            (guid_to_db(local), 2),
            (guid_to_db(linked), 3),
        ];
        want.sort();
        assert_eq!(got, want);
        assert_eq!(
            backfill_alternate_version_links(&db).await.expect("again"),
            0
        );
    }
}

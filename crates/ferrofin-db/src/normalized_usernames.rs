//! The `Users.NormalizedUsername` lookup key (Jellyfin 12.0) and the boot-time
//! repair that rewrites keys the schema migration folded ASCII-only.
//!
//! 12.0 stores `Username.ToUpperInvariant()` in the column and matches every
//! by-name lookup on it exactly (`User.cs:28-29`, `UserManager.cs:163/185/348`).
//! The migration that added the column filled it with SQL `upper()`, which
//! only folds ASCII: a user named `münchen` was stored as `MüNCHEN`, and every
//! lookup — login included — compares against `MÜNCHEN` and misses. This is
//! the ONE spelling of the key ([`normalized_username`]); the manager's writes
//! and the repair both go through it, so a repaired row and a freshly written
//! one cannot disagree.

use crate::{Database, Result};

/// The `FerrofinMeta` key recording that [`repair_normalized_usernames`] ran.
pub const META_KEY: &str = "normalized_usernames_v12";

/// The stored lookup key for a username: .NET `ToUpperInvariant` (the simple
/// one-to-one uppercase mapping — `ß` stays `ß`).
#[must_use]
pub fn normalized_username(username: &str) -> String {
    ferrofin_util::string_extensions::upper_invariant(username)
}

/// One-shot startup pass: rewrites every `NormalizedUsername` that is not
/// [`normalized_username`] of its `Username`, recording completion under
/// [`META_KEY`] so later boots skip it. Returns how many rows were rewritten.
///
/// Rows that already agree (every ASCII name, and every row Jellyfin 12.0 or
/// a current Ferrofin wrote) are left untouched. The marker is written inside
/// the rewrite transaction, so a failure simply retries on the next boot.
///
/// # Errors
/// Returns [`DbError::Sqlx`](crate::DbError::Sqlx) if a query or the
/// transaction fails.
pub async fn repair_normalized_usernames(db: &Database) -> Result<u64> {
    if db.meta_get(META_KEY).await?.as_deref() == Some("1") {
        return Ok(0);
    }

    let rows: Vec<(String, String, String)> =
        sqlx::query_as(r#"SELECT "Id", "Username", "NormalizedUsername" FROM "Users""#)
            .fetch_all(db.pool())
            .await?;

    let mut tx = db.writer().begin().await?;
    let mut repaired: u64 = 0;
    for (id, username, stored) in rows {
        let want = normalized_username(&username);
        if want == stored {
            continue;
        }
        sqlx::query(r#"UPDATE "Users" SET "NormalizedUsername" = ?2 WHERE "Id" = ?1"#)
            .bind(&id)
            .bind(&want)
            .execute(&mut *tx)
            .await?;
        repaired += 1;
    }
    sqlx::query(
        r#"INSERT INTO "FerrofinMeta" ("Key", "Value") VALUES (?1, '1')
           ON CONFLICT("Key") DO UPDATE SET "Value" = excluded."Value""#,
    )
    .bind(META_KEY)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(repaired)
}

#[cfg(test)]
mod tests {
    use super::{META_KEY, normalized_username, repair_normalized_usernames};
    use crate::Database;

    /// Inserts a `Users` row keyed the way migration 0030's `upper()` did.
    async fn seed(db: &Database, id: &str, username: &str) {
        sqlx::query(
            r#"INSERT INTO "Users"
               ("Id", "AuthenticationProviderId", "DisplayCollectionsView",
                "DisplayMissingEpisodes", "EnableAutoLogin", "EnableLocalPassword",
                "EnableNextEpisodeAutoPlay", "EnableUserPreferenceAccess",
                "HidePlayedInLatest", "InternalId", "InvalidLoginAttemptCount",
                "MaxActiveSessions", "MustUpdatePassword",
                "PasswordResetProviderId", "PlayDefaultAudioTrack",
                "RememberAudioSelections", "RememberSubtitleSelections",
                "RowVersion", "SubtitleMode", "SyncPlayAccess", "Username", "NormalizedUsername")
               VALUES (?1, '', 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, '', 1, 1, 1, 0, 0, 0, ?2, upper(?2))"#,
        )
        .bind(id)
        .bind(username)
        .execute(db.writer())
        .await
        .expect("seed user");
    }

    async fn stored(db: &Database, username: &str) -> String {
        sqlx::query_scalar(r#"SELECT "NormalizedUsername" FROM "Users" WHERE "Username" = ?1"#)
            .bind(username)
            .fetch_one(db.pool())
            .await
            .expect("stored key")
    }

    #[test]
    fn the_key_is_the_invariant_uppercase() {
        assert_eq!(normalized_username("alice"), "ALICE");
        assert_eq!(normalized_username("münchen"), "MÜNCHEN");
        assert_eq!(normalized_username("straße"), "STRAßE");
    }

    /// Non-ASCII keys the migration folded wrong are rewritten, agreeing rows
    /// are left alone, and the marker makes the pass a no-op afterwards.
    #[tokio::test]
    async fn repair_rewrites_ascii_folded_keys_once() {
        let db = Database::connect_in_memory().await.expect("connect");
        db.run_migrations().await.expect("migrate");
        seed(&db, "00000000-0000-0000-0000-000000000051", "münchen").await;
        seed(&db, "00000000-0000-0000-0000-000000000052", "þór").await;
        seed(&db, "00000000-0000-0000-0000-000000000053", "alice").await;
        assert_eq!(
            stored(&db, "münchen").await,
            "MüNCHEN",
            "the ASCII-only fold"
        );

        assert_eq!(repair_normalized_usernames(&db).await.expect("repair"), 2);
        assert_eq!(stored(&db, "münchen").await, "MÜNCHEN");
        assert_eq!(stored(&db, "þór").await, "ÞÓR");
        assert_eq!(stored(&db, "alice").await, "ALICE");
        assert_eq!(
            db.meta_get(META_KEY).await.expect("meta").as_deref(),
            Some("1")
        );

        // Second boot: nothing to do, even if a row were wrong again.
        assert_eq!(repair_normalized_usernames(&db).await.expect("again"), 0);
    }
}

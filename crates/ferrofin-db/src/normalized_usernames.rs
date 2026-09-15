//! Code migration for Jellyfin-compatible username keys. SQL `upper()` only
//! handles ASCII; ICU must populate these values before uniqueness is enforced.

use crate::{DbError, Result};
use ferrofin_util::string_extensions::upper_invariant;
use sqlx::{Connection, SqliteConnection};

const MARKER: &str = "normalized_usernames_icu_v1";

/// Converges old Ferrofin and adopted Jellyfin schemas before serving requests.
/// The column, values, index and completion marker change in one transaction.
/// Existing account IDs and all other user fields remain untouched.
///
/// # Errors
/// Returns an actionable collision error without changing username state when
/// two accounts normalize to the same key, or a database error on failure.
pub async fn migrate(conn: &mut SqliteConnection) -> Result<()> {
    let mut tx = conn.begin().await?;
    let done: Option<String> =
        sqlx::query_scalar(r#"SELECT "Value" FROM "FerrofinMeta" WHERE "Key" = ?1"#)
            .bind(MARKER)
            .fetch_optional(&mut *tx)
            .await?;
    if done.as_deref() == Some("1") {
        return Ok(());
    }
    let users: Vec<(String, String)> =
        sqlx::query_as(r#"SELECT "Id", "Username" FROM "Users" ORDER BY "Id""#)
            .fetch_all(&mut *tx)
            .await?;
    let mut keys = std::collections::HashMap::new();
    for (id, name) in &users {
        if let Some(first) = keys.insert(upper_invariant(name), (id.clone(), name.clone())) {
            return Err(DbError::UsernameCollision {
                first,
                second: (id.clone(), name.clone()),
            });
        }
    }
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM pragma_table_info('Users') WHERE name = 'NormalizedUsername'",
    )
    .fetch_optional(&mut *tx)
    .await?;
    if exists.is_none() {
        sqlx::query(
            r#"ALTER TABLE "Users" ADD COLUMN "NormalizedUsername" TEXT NOT NULL DEFAULT ''"#,
        )
        .execute(&mut *tx)
        .await?;
    }
    // Rebuild inside the transaction so corrections can swap stale keys.
    sqlx::query(r#"DROP INDEX IF EXISTS "IX_Users_NormalizedUsername""#)
        .execute(&mut *tx)
        .await?;
    for (id, name) in &users {
        sqlx::query(r#"UPDATE "Users" SET "NormalizedUsername" = ?2 WHERE "Id" = ?1"#)
            .bind(id)
            .bind(upper_invariant(name))
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        r#"CREATE UNIQUE INDEX "IX_Users_NormalizedUsername" ON "Users" ("NormalizedUsername")"#,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(r#"INSERT INTO "FerrofinMeta" ("Key", "Value") VALUES (?1, '1')"#)
        .bind(MARKER)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    async fn fixture(has_column: bool) -> SqliteConnection {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "FerrofinMeta" ("Key" TEXT PRIMARY KEY, "Value" TEXT NOT NULL);
            CREATE TABLE "Users" ("Id" TEXT PRIMARY KEY, "Username" TEXT UNIQUE,
                "Password" TEXT, "Permission" INTEGER);
            INSERT INTO "Users" VALUES ('a', 'münchen', 'password-hash-a', 1),
                ('b', 'ı', 'password-hash-b', 0);
        "#,
        )
        .execute(&mut conn)
        .await
        .unwrap();
        if has_column {
            sqlx::raw_sql(
                r#"
                ALTER TABLE "Users" ADD COLUMN "NormalizedUsername" TEXT NOT NULL DEFAULT '';
                UPDATE "Users" SET "NormalizedUsername" = upper("Username");
                CREATE UNIQUE INDEX "IX_Users_NormalizedUsername" ON "Users" ("NormalizedUsername");
            "#,
            )
            .execute(&mut conn)
            .await
            .unwrap();
        }
        conn
    }

    #[rstest]
    #[case(false)]
    #[case(true)]
    #[tokio::test]
    async fn migration_preserves_accounts_and_enforces_unicode_uniqueness(
        #[case] has_column: bool,
    ) {
        let mut conn = fixture(has_column).await;
        migrate(&mut conn).await.unwrap();
        let rows: Vec<(String, String, String, String, i64)> = sqlx::query_as(
            r#"SELECT "Id", "Username", "NormalizedUsername", "Password", "Permission" FROM "Users" ORDER BY "Id""#
        ).fetch_all(&mut conn).await.unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    "a".into(),
                    "münchen".into(),
                    "MÜNCHEN".into(),
                    "password-hash-a".into(),
                    1
                ),
                (
                    "b".into(),
                    "ı".into(),
                    "ı".into(),
                    "password-hash-b".into(),
                    0
                ),
            ]
        );
        let duplicate = sqlx::query(r#"INSERT INTO "Users" ("Id", "Username", "NormalizedUsername") VALUES ('c', 'MÜNCHEN', 'MÜNCHEN')"#)
            .execute(&mut conn).await.unwrap_err();
        assert!(duplicate.as_database_error().unwrap().is_unique_violation());
        migrate(&mut conn).await.unwrap();
        let markers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM FerrofinMeta")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(markers, 1);
    }

    #[rstest]
    #[case(false)]
    #[case(true)]
    #[tokio::test]
    async fn failed_backfill_rolls_back_schema_index_and_completion(#[case] has_column: bool) {
        let mut conn = fixture(has_column).await;
        sqlx::query("CREATE TRIGGER fail_backfill BEFORE UPDATE ON Users BEGIN SELECT RAISE(ABORT, 'injected failure'); END")
            .execute(&mut conn).await.unwrap();
        let before: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT name, sql FROM sqlite_master ORDER BY name")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert!(migrate(&mut conn).await.is_err());
        let after: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT name, sql FROM sqlite_master ORDER BY name")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(before, after);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM FerrofinMeta")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(count, 0);
        sqlx::query("DROP TRIGGER fail_backfill")
            .execute(&mut conn)
            .await
            .unwrap();
        migrate(&mut conn).await.unwrap();
    }

    #[rstest]
    #[case(false)]
    #[case(true)]
    #[tokio::test]
    async fn collisions_leave_username_schema_values_and_marker_untouched(
        #[case] has_column: bool,
    ) {
        let mut conn = fixture(has_column).await;
        sqlx::query("UPDATE Users SET Username = 'MÜNCHEN' WHERE Id = 'b'")
            .execute(&mut conn)
            .await
            .unwrap();
        let before: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT name, sql FROM sqlite_master ORDER BY name")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        let error = migrate(&mut conn).await.unwrap_err();
        assert!(matches!(error, DbError::UsernameCollision { .. }));
        let message = error.to_string();
        assert!(message.contains("münchen") && message.contains("MÜNCHEN"));
        let after: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT name, sql FROM sqlite_master ORDER BY name")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(before, after);
        let markers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM FerrofinMeta")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(markers, 0);
        if has_column {
            let key: String =
                sqlx::query_scalar("SELECT NormalizedUsername FROM Users WHERE Id = 'a'")
                    .fetch_one(&mut conn)
                    .await
                    .unwrap();
            assert_eq!(key, "MüNCHEN");
        }
        // After the operator resolves the conflict, migration retries normally.
        sqlx::query("UPDATE Users SET Username = 'different' WHERE Id = 'b'")
            .execute(&mut conn)
            .await
            .unwrap();
        migrate(&mut conn).await.unwrap();
    }
}

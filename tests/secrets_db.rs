//! Integration tests coupling the secrets store to the db module's schema.
//!
//! The secrets store carries its own `CREATE TABLE IF NOT EXISTS secrets ...`
//! (so it is testable without the migrations), which duplicates the DDL owned
//! by the db module's migration. These tests catch drift between the two
//! mechanically: if the store's DDL ever diverges incompatibly from the
//! migration's table, they break.

use orchestrator::db::Db;
use orchestrator::secrets::SecretStore;
use r2d2_sqlite::SqliteConnectionManager;

/// Column layout of a table as reported by SQLite:
/// (cid, name, declared type, notnull, is primary key).
type ColumnInfo = (i64, String, String, bool, bool);

fn secrets_table_info(conn: &rusqlite::Connection) -> Vec<ColumnInfo> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(secrets)")
        .expect("prepare table_info");
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?.to_ascii_uppercase(),
                row.get::<_, i64>(3)? != 0,
                row.get::<_, i64>(5)? != 0,
            ))
        })
        .expect("query table_info");
    rows.collect::<Result<Vec<_>, _>>()
        .expect("read table_info")
}

fn store_pool(db_path: &std::path::Path) -> r2d2::Pool<SqliteConnectionManager> {
    r2d2::Pool::builder()
        .max_size(2)
        .build(SqliteConnectionManager::file(db_path))
        .expect("build pool")
}

/// The store must work against the table created by the db migration: open the
/// database through `Db::open` first (running migrations), then run the store
/// against the same file. If the store's DDL assumptions (column names/types)
/// drift from the migration's table, the round-trip fails.
#[test]
fn secret_store_round_trips_on_migrated_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("orchestrator.db");

    let db = Db::open(&db_path).expect("open db (runs migrations)");

    let store = SecretStore::open(&dir.path().join("secrets.key"), store_pool(&db_path))
        .expect("open secret store");
    store.set("API_TOKEN", "hunter2").expect("set secret");
    assert_eq!(
        store.get("API_TOKEN").expect("get secret").as_deref(),
        Some("hunter2")
    );

    // The row must live in the migration-owned table, visible through Db too.
    let conn = db.conn().expect("db conn");
    let (name, created_at, updated_at): (String, String, String) = conn
        .query_row(
            "SELECT name, created_at, updated_at FROM secrets WHERE name = ?1",
            ["API_TOKEN"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("row visible through migrated schema");
    assert_eq!(name, "API_TOKEN");
    assert!(!created_at.is_empty());
    assert!(!updated_at.is_empty());
}

/// DDL drift guard: the table the store creates for itself (fresh DB, no
/// migrations) must have exactly the same column layout as the table the db
/// migration creates. Compares `PRAGMA table_info` output column by column.
#[test]
fn store_ddl_matches_migration_ddl() {
    let dir = tempfile::tempdir().expect("tempdir");

    // Table as created by the db module's migration.
    let migrated_path = dir.path().join("migrated.db");
    let db = Db::open(&migrated_path).expect("open db (runs migrations)");
    let migrated_info = secrets_table_info(&db.conn().expect("db conn"));
    assert!(
        !migrated_info.is_empty(),
        "migration did not create a secrets table"
    );

    // Table as created by the store's own CREATE TABLE IF NOT EXISTS.
    let store_path = dir.path().join("store-only.db");
    let pool = store_pool(&store_path);
    let _store = SecretStore::open(&dir.path().join("secrets.key"), pool.clone())
        .expect("open secret store on fresh db");
    let store_info = secrets_table_info(&pool.get().expect("pool conn"));

    assert_eq!(
        store_info, migrated_info,
        "secrets table DDL drift: the store's CREATE TABLE (src/secrets.rs) no \
         longer matches the db migration's secrets table (src/db.rs)"
    );
}

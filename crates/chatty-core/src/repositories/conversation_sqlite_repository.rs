use std::path::PathBuf;
use std::sync::Arc;

use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tracing::info;

use super::conversation_repository::{
    BoxFuture, ConversationData, ConversationMetadata, ConversationRepository,
};
use super::error::{RepositoryError, RepositoryResult};

/// Migrations applied in order. Each entry is (version, sql).
/// To add a new migration: append a tuple with the next version number and its SQL.
/// Never edit or remove existing entries — existing databases depend on them.
const MIGRATIONS: &[(i64, &str)] = &[
    (
        1,
        "CREATE TABLE IF NOT EXISTS conversations (
        id                   TEXT    PRIMARY KEY,
        title                TEXT    NOT NULL DEFAULT '',
        model_id             TEXT    NOT NULL DEFAULT '',
        message_history      TEXT    NOT NULL DEFAULT '[]',
        system_traces        TEXT    NOT NULL DEFAULT '[]',
        token_usage          TEXT    NOT NULL DEFAULT '{}',
        attachment_paths     TEXT    NOT NULL DEFAULT '[]',
        message_timestamps   TEXT    NOT NULL DEFAULT '[]',
        message_feedback     TEXT    NOT NULL DEFAULT '[]',
        regeneration_records TEXT    NOT NULL DEFAULT '[]',
        total_cost           REAL    NOT NULL DEFAULT 0.0,
        created_at           INTEGER NOT NULL DEFAULT 0,
        updated_at           INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_conversations_updated_at
        ON conversations (updated_at DESC);",
    ),
    (2, "ALTER TABLE conversations ADD COLUMN working_dir TEXT;"),
    (
        3,
        "ALTER TABLE conversations ADD COLUMN agent_task_snapshot TEXT;",
    ),
    // AGE-298: where the conversation's turns run. NULL means local, so every
    // row that predates the column keeps its meaning without a backfill.
    (4, "ALTER TABLE conversations ADD COLUMN mode TEXT;"),
];

/// Creates the database directory, opens the pool and applies any pending
/// migrations. Called at most once per [`LazyPool`], from whichever query
/// runs first.
async fn open_pool(db_path: PathBuf) -> RepositoryResult<SqlitePool> {
    if let Some(parent) = db_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    ConversationSqliteRepository::run_migrations(&pool).await?;

    info!(path = %db_path.display(), "Opened SQLite conversation database");

    Ok(pool)
}

/// A connection pool that is opened on first use rather than on construction.
///
/// Opening it creates the database file, runs migrations and pays several
/// `fsync`s. Doing that eagerly put disk I/O on the desktop binary's
/// pre-`Application::run` path, where it delayed the first frame for work
/// nothing had asked for yet (AGE-161). Every repository method goes through
/// [`LazyPool::get`], so the cost now lands on the first query — in the
/// desktop app the sidebar's `load_metadata`, which already runs inside a
/// spawned task.
///
/// Concurrent first callers are serialised by `OnceCell`, so the pool is
/// opened (and the migrations applied) exactly once.
#[derive(Clone)]
struct LazyPool {
    db_path: PathBuf,
    cell: Arc<OnceCell<SqlitePool>>,
}

impl LazyPool {
    fn new(db_path: PathBuf) -> Self {
        Self {
            db_path,
            cell: Arc::new(OnceCell::new()),
        }
    }

    /// The pool, opening it if this is the first call.
    ///
    /// A failed open is not cached: the next query retries, so a transient
    /// failure (e.g. the config directory not yet writable) doesn't poison
    /// the repository for the rest of the session.
    async fn get(&self) -> RepositoryResult<SqlitePool> {
        self.cell
            .get_or_try_init(|| open_pool(self.db_path.clone()))
            .await
            .cloned()
    }
}

/// SQLite-backed repository for conversations.
///
/// Uses WAL journal mode for concurrent reads during background saves.
/// The pool is opened lazily — see [`LazyPool`] — and is internally
/// reference-counted and cheap to clone.
pub struct ConversationSqliteRepository {
    pool: LazyPool,
}

impl ConversationSqliteRepository {
    /// Bind the repository to the SQLite database at the platform-specific
    /// config path, without touching the disk.
    ///
    /// Costs a `dirs::config_dir()` lookup and nothing else, so it is safe to
    /// call on a latency-sensitive path; the pool opens on the first query.
    pub fn deferred() -> RepositoryResult<Self> {
        Ok(Self {
            pool: LazyPool::new(Self::db_path()?),
        })
    }

    /// As [`Self::deferred`], at an arbitrary path. Test-only: used by unit
    /// tests and the `store_conformance` suite exported behind `test-support`
    /// to run against an isolated database per test.
    #[cfg(any(test, feature = "test-support"))]
    pub fn deferred_with_path(db_path: PathBuf) -> Self {
        Self {
            pool: LazyPool::new(db_path),
        }
    }

    /// Create the schema_version table if absent, then apply any pending migrations.
    async fn run_migrations(pool: &SqlitePool) -> RepositoryResult<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER NOT NULL
            )",
        )
        .execute(pool)
        .await?;

        // Seed version 0 if the table is empty (fresh database).
        sqlx::query("INSERT INTO schema_version (version) SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM schema_version)")
            .execute(pool)
            .await?;

        let current: i64 = sqlx::query_scalar("SELECT version FROM schema_version")
            .fetch_one(pool)
            .await?;

        for (version, sql) in MIGRATIONS {
            if *version > current {
                info!(version, "Applying schema migration");
                // sqlx doesn't support multiple statements in a single query call,
                // so split on ';' and execute each statement individually.
                for statement in sql.split(';') {
                    let trimmed = statement.trim();
                    if !trimmed.is_empty() {
                        // Migration SQL is a compile-time constant split on ';',
                        // not user input. sqlx 0.9 requires AssertSqlSafe for
                        // non-'static query strings.
                        sqlx::query(sqlx::AssertSqlSafe(trimmed.to_string()))
                            .execute(pool)
                            .await?;
                    }
                }
                sqlx::query("UPDATE schema_version SET version = ?")
                    .bind(version)
                    .execute(pool)
                    .await?;
            }
        }

        Ok(())
    }

    fn db_path() -> RepositoryResult<PathBuf> {
        dirs::config_dir()
            .ok_or_else(|| RepositoryError::InitializationError {
                message: "Cannot find config directory".into(),
            })
            .map(|p| p.join("chatty").join("conversations.db"))
    }
}

impl Clone for ConversationSqliteRepository {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

impl ConversationRepository for ConversationSqliteRepository {
    fn load_metadata(&self) -> BoxFuture<'static, RepositoryResult<Vec<ConversationMetadata>>> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let pool = pool.get().await?;
            let rows = sqlx::query(
                "SELECT id, title, total_cost, updated_at, mode
                 FROM conversations
                 ORDER BY updated_at DESC",
            )
            .fetch_all(&pool)
            .await?;

            let metadata = rows
                .iter()
                .map(|row| ConversationMetadata {
                    id: row.get("id"),
                    title: row.get("title"),
                    total_cost: row.get("total_cost"),
                    updated_at: row.get("updated_at"),
                    mode: row.get("mode"),
                })
                .collect();

            Ok(metadata)
        })
    }

    fn load_one(&self, id: &str) -> BoxFuture<'static, RepositoryResult<Option<ConversationData>>> {
        let pool = self.pool.clone();
        let id = id.to_string();
        Box::pin(async move {
            let pool = pool.get().await?;
            let row = sqlx::query(
                "SELECT id, title, model_id, message_history, system_traces, token_usage,
                        attachment_paths, message_timestamps, message_feedback,
                        regeneration_records, created_at, updated_at, working_dir, agent_task_snapshot,
                        mode
                 FROM conversations
                 WHERE id = ?",
            )
            .bind(&id)
            .fetch_optional(&pool)
            .await?;

            Ok(row.map(|r| ConversationData {
                id: r.get("id"),
                title: r.get("title"),
                model_id: r.get("model_id"),
                message_history: r.get("message_history"),
                system_traces: r.get("system_traces"),
                token_usage: r.get("token_usage"),
                attachment_paths: r.get("attachment_paths"),
                message_timestamps: r.get("message_timestamps"),
                message_feedback: r.get("message_feedback"),
                regeneration_records: r.get("regeneration_records"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
                working_dir: r.get("working_dir"),
                agent_task_snapshot: r.get("agent_task_snapshot"),
                mode: r.get("mode"),
            }))
        })
    }

    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<ConversationData>>> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let pool = pool.get().await?;
            let rows = sqlx::query(
                "SELECT id, title, model_id, message_history, system_traces, token_usage,
                        attachment_paths, message_timestamps, message_feedback,
                        regeneration_records, created_at, updated_at, working_dir, agent_task_snapshot,
                        mode
                 FROM conversations
                 ORDER BY updated_at DESC",
            )
            .fetch_all(&pool)
            .await?;

            Ok(rows
                .iter()
                .map(|r| ConversationData {
                    id: r.get("id"),
                    title: r.get("title"),
                    model_id: r.get("model_id"),
                    message_history: r.get("message_history"),
                    system_traces: r.get("system_traces"),
                    token_usage: r.get("token_usage"),
                    attachment_paths: r.get("attachment_paths"),
                    message_timestamps: r.get("message_timestamps"),
                    message_feedback: r.get("message_feedback"),
                    regeneration_records: r.get("regeneration_records"),
                    created_at: r.get("created_at"),
                    updated_at: r.get("updated_at"),
                    working_dir: r.get("working_dir"),
                    agent_task_snapshot: r.get("agent_task_snapshot"),
                    mode: r.get("mode"),
                })
                .collect())
        })
    }

    fn save(&self, _id: &str, data: ConversationData) -> BoxFuture<'static, RepositoryResult<()>> {
        let pool = self.pool.clone();
        let total_cost = data.total_cost();
        Box::pin(async move {
            let pool = pool.get().await?;
            sqlx::query(
                "INSERT INTO conversations
                    (id, title, model_id, message_history, system_traces, token_usage,
                     attachment_paths, message_timestamps, message_feedback,
                     regeneration_records, total_cost, created_at, updated_at, working_dir, agent_task_snapshot,
                     mode)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                 ON CONFLICT(id) DO UPDATE SET
                    title                = excluded.title,
                    model_id             = excluded.model_id,
                    message_history      = excluded.message_history,
                    system_traces        = excluded.system_traces,
                    token_usage          = excluded.token_usage,
                    attachment_paths     = excluded.attachment_paths,
                    message_timestamps   = excluded.message_timestamps,
                    message_feedback     = excluded.message_feedback,
                    regeneration_records = excluded.regeneration_records,
                    total_cost           = excluded.total_cost,
                    updated_at           = excluded.updated_at,
                    working_dir          = excluded.working_dir,
                    agent_task_snapshot  = excluded.agent_task_snapshot,
                    mode                 = excluded.mode",
            )
            .bind(&data.id)
            .bind(&data.title)
            .bind(&data.model_id)
            .bind(&data.message_history)
            .bind(&data.system_traces)
            .bind(&data.token_usage)
            .bind(&data.attachment_paths)
            .bind(&data.message_timestamps)
            .bind(&data.message_feedback)
            .bind(&data.regeneration_records)
            .bind(total_cost)
            .bind(data.created_at)
            .bind(data.updated_at)
            .bind(&data.working_dir)
            .bind(&data.agent_task_snapshot)
            .bind(&data.mode)
            .execute(&pool)
            .await?;

            Ok(())
        })
    }

    fn delete(&self, id: &str) -> BoxFuture<'static, RepositoryResult<()>> {
        let pool = self.pool.clone();
        let id = id.to_string();
        Box::pin(async move {
            let pool = pool.get().await?;
            sqlx::query("DELETE FROM conversations WHERE id = ?")
                .bind(&id)
                .execute(&pool)
                .await?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::store_conformance::sample_conversation;

    /// The point of `deferred`: constructing the repository must not create
    /// the database, its directory, or a connection — that work belongs to
    /// the first query (AGE-161).
    #[tokio::test]
    async fn deferred_touches_no_disk_until_the_first_query() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_dir = dir.path().join("chatty");
        let db_path = db_dir.join("conversations.db");

        let repo = ConversationSqliteRepository::deferred_with_path(db_path.clone());
        assert!(
            !db_dir.exists(),
            "constructing the repository must not create the database directory"
        );
        assert!(
            !repo.pool.cell.initialized(),
            "constructing the repository must not open a connection pool"
        );

        let metadata = repo
            .load_metadata()
            .await
            .expect("the first query opens the pool and applies migrations");

        assert!(metadata.is_empty(), "a fresh database has no conversations");
        assert!(
            db_path.exists(),
            "the first query must create the database file"
        );
        assert!(
            repo.pool.cell.initialized(),
            "the first query must leave the pool open for reuse"
        );
    }

    /// Clones share one `OnceCell`, so a query through any clone opens the
    /// pool for all of them. A per-clone cell would open a second pool (and
    /// re-run migrations) behind the app's back.
    #[tokio::test]
    async fn clones_share_one_lazily_opened_pool() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo =
            ConversationSqliteRepository::deferred_with_path(dir.path().join("conversations.db"));
        let clone = repo.clone();

        clone
            .save("conv-a", sample_conversation("conv-a", "First", 1_000))
            .await
            .expect("save through the clone");

        assert!(
            repo.pool.cell.initialized(),
            "opening the pool through a clone must initialise the original's cell"
        );
        assert!(
            repo.load_one("conv-a")
                .await
                .expect("load through the original")
                .is_some(),
            "both handles must see the same database"
        );
    }
}

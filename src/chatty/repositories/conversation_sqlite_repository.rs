use std::path::PathBuf;

use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use tracing::info;

use super::conversation_repository::{
    BoxFuture, ConversationData, ConversationMetadata, ConversationRepository,
};
use super::error::{RepositoryError, RepositoryResult};

/// Migrations applied in order. Each entry is (version, sql).
/// To add a new migration: append a tuple with the next version number and its SQL.
/// Never edit or remove existing entries — existing databases depend on them.
const MIGRATIONS: &[(i64, &str)] = &[(
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
)];

/// SQLite-backed repository for conversations.
///
/// Uses WAL journal mode for concurrent reads during background saves.
/// `SqlitePool` is internally reference-counted and cheap to clone.
pub struct ConversationSqliteRepository {
    pool: SqlitePool,
}

impl ConversationSqliteRepository {
    /// Open (or create) the SQLite database at the platform-specific config path.
    pub async fn new() -> RepositoryResult<Self> {
        let db_path = Self::db_path()?;

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

        Self::run_migrations(&pool).await?;

        info!(path = %db_path.display(), "Opened SQLite conversation database");

        Ok(Self { pool })
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
                        sqlx::query(trimmed).execute(pool).await?;
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
            let rows = sqlx::query(
                "SELECT id, title, total_cost, updated_at
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
                })
                .collect();

            Ok(metadata)
        })
    }

    fn load_one(&self, id: &str) -> BoxFuture<'static, RepositoryResult<Option<ConversationData>>> {
        let pool = self.pool.clone();
        let id = id.to_string();
        Box::pin(async move {
            let row = sqlx::query(
                "SELECT id, title, model_id, message_history, system_traces, token_usage,
                        attachment_paths, message_timestamps, message_feedback,
                        regeneration_records, created_at, updated_at
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
            }))
        })
    }

    fn load_all(&self) -> BoxFuture<'static, RepositoryResult<Vec<ConversationData>>> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT id, title, model_id, message_history, system_traces, token_usage,
                        attachment_paths, message_timestamps, message_feedback,
                        regeneration_records, created_at, updated_at
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
                })
                .collect())
        })
    }

    fn save(&self, _id: &str, data: ConversationData) -> BoxFuture<'static, RepositoryResult<()>> {
        let pool = self.pool.clone();
        let total_cost = data.total_cost();
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO conversations
                    (id, title, model_id, message_history, system_traces, token_usage,
                     attachment_paths, message_timestamps, message_feedback,
                     regeneration_records, total_cost, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
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
                    updated_at           = excluded.updated_at",
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
            .execute(&pool)
            .await?;

            Ok(())
        })
    }

    fn delete(&self, id: &str) -> BoxFuture<'static, RepositoryResult<()>> {
        let pool = self.pool.clone();
        let id = id.to_string();
        Box::pin(async move {
            sqlx::query("DELETE FROM conversations WHERE id = ?")
                .bind(&id)
                .execute(&pool)
                .await?;
            Ok(())
        })
    }
}

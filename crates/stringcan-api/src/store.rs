use sqlx::types::Json;
use sqlx::{PgConnection, Postgres, Transaction};
use stringcan_core::{Command, Config, Engine, UserId};
use uuid::Uuid;

use crate::error::ApiError;

/// Bump when engine behaviour changes. Old rows must stay replayable, so a new
/// version needs the previous rules kept alongside (see CLAUDE.md).
pub const RULES_VERSION: i32 = 1;

pub struct Loaded {
    pub engine: Engine,
    pub next_seq: i64,
    pub currency: String,
}

/// Serialises writers per tenant: replay-then-append is only race free while
/// this row lock is held.
pub async fn load_locked(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
) -> Result<Loaded, ApiError> {
    load(tx, tenant, true).await
}

/// Consistent snapshot without blocking writers.
pub async fn begin_read(pool: &sqlx::PgPool) -> Result<Transaction<'_, Postgres>, ApiError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

pub async fn load_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
) -> Result<Loaded, ApiError> {
    load(tx, tenant, false).await
}

async fn load(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    lock: bool,
) -> Result<Loaded, ApiError> {
    let sql = if lock {
        "SELECT engine_config, first_upkeep_cents, currency FROM tenants WHERE id = $1 FOR UPDATE"
    } else {
        "SELECT engine_config, first_upkeep_cents, currency FROM tenants WHERE id = $1"
    };
    let (cfg, first, currency): (Json<Config>, i64, String) = sqlx::query_as(sql)
        .bind(tenant)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(ApiError::NotFound)?;
    let snapshot = load_snapshot_state(tx, tenant).await?;
    let (mut engine, mut next_seq) = match snapshot {
        Some(found) => found,
        None => (Engine::new(cfg.0, first)?, 0),
    };

    let rows: Vec<(i64, i32, Json<Command>)> = sqlx::query_as(
        "SELECT seq, rules_version, command FROM commands WHERE tenant_id = $1 AND seq >= $2 ORDER BY seq",
    )
    .bind(tenant)
    .bind(next_seq)
    .fetch_all(&mut **tx)
    .await?;
    for (seq, version, cmd) in rows {
        if version != RULES_VERSION {
            return Err(ApiError::Internal(format!(
                "command {seq} has unsupported rules version {version}"
            )));
        }
        if seq != next_seq {
            return Err(ApiError::Internal(format!(
                "command log gap: expected {next_seq}, found {seq}"
            )));
        }
        engine
            .execute(&cmd.0)
            .map_err(|e| ApiError::Internal(format!("replay diverged at command {seq}: {e}")))?;
        next_seq = seq + 1;
    }
    Ok(Loaded {
        engine,
        next_seq,
        currency,
    })
}

/// Latest usable snapshot, or None to replay from the start. A bad snapshot is
/// logged and skipped, never trusted.
async fn load_snapshot_state(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
) -> Result<Option<(Engine, i64)>, ApiError> {
    let row: Option<(i64, serde_json::Value)> = sqlx::query_as(
        "SELECT next_seq, state FROM snapshots WHERE tenant_id = $1 AND rules_version = $2
         ORDER BY next_seq DESC LIMIT 1",
    )
    .bind(tenant)
    .bind(RULES_VERSION)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((next_seq, state)) = row else {
        return Ok(None);
    };
    match serde_json::from_value::<Engine>(state) {
        Ok(engine) if engine.audit().is_ok() => Ok(Some((engine, next_seq))),
        _ => {
            tracing::warn!(%tenant, next_seq, "ignoring invalid snapshot; replaying full log");
            Ok(None)
        }
    }
}

pub async fn append(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    loaded: &mut Loaded,
    cmd: &Command,
    processor_ref: Option<&str>,
    snapshot_every: i64,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO commands (tenant_id, seq, rules_version, command, processor_ref) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(tenant)
    .bind(loaded.next_seq)
    .bind(RULES_VERSION)
    .bind(Json(cmd))
    .bind(processor_ref)
    .execute(&mut **tx)
    .await?;
    loaded.next_seq += 1;
    if snapshot_every > 0 && loaded.next_seq % snapshot_every == 0 {
        sqlx::query(
            "INSERT INTO snapshots (tenant_id, next_seq, rules_version, state) VALUES ($1, $2, $3, $4)
             ON CONFLICT DO NOTHING",
        )
        .bind(tenant)
        .bind(loaded.next_seq)
        .bind(RULES_VERSION)
        .bind(Json(&loaded.engine))
        .execute(&mut **tx)
        .await?;
        sqlx::query("DELETE FROM snapshots WHERE tenant_id = $1 AND next_seq < $2")
            .bind(tenant)
            .bind(loaded.next_seq)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

pub async fn find_user(
    conn: &mut PgConnection,
    tenant: Uuid,
    external_ref: &str,
) -> Result<Option<UserId>, ApiError> {
    let id: Option<i64> = sqlx::query_scalar(
        "SELECT user_id FROM subscribers WHERE tenant_id = $1 AND external_ref = $2",
    )
    .bind(tenant)
    .bind(external_ref)
    .fetch_optional(conn)
    .await?;
    Ok(id.map(|i| i as UserId))
}

/// Call only while holding the tenant row lock; the max+1 allocation relies on it.
pub async fn ensure_user(
    conn: &mut PgConnection,
    tenant: Uuid,
    external_ref: &str,
) -> Result<UserId, ApiError> {
    if let Some(id) = find_user(&mut *conn, tenant, external_ref).await? {
        return Ok(id);
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO subscribers (tenant_id, user_id, external_ref)
         VALUES ($1, COALESCE((SELECT MAX(user_id) FROM subscribers WHERE tenant_id = $1), 0) + 1, $2)
         RETURNING user_id",
    )
    .bind(tenant)
    .bind(external_ref)
    .fetch_one(conn)
    .await?;
    Ok(id as UserId)
}

pub async fn external_ref(
    conn: &mut PgConnection,
    tenant: Uuid,
    user: UserId,
) -> Result<String, ApiError> {
    sqlx::query_scalar("SELECT external_ref FROM subscribers WHERE tenant_id = $1 AND user_id = $2")
        .bind(tenant)
        .bind(user as i64)
        .fetch_optional(conn)
        .await?
        .ok_or(ApiError::NotFound)
}

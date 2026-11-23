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
    let mut engine = Engine::new(cfg.0, first)?;

    let rows: Vec<(i64, i32, Json<Command>)> = sqlx::query_as(
        "SELECT seq, rules_version, command FROM commands WHERE tenant_id = $1 ORDER BY seq",
    )
    .bind(tenant)
    .fetch_all(&mut **tx)
    .await?;
    let mut next_seq = 0;
    for (seq, version, cmd) in rows {
        if version != RULES_VERSION {
            return Err(ApiError::Internal(format!(
                "command {seq} has unsupported rules version {version}"
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

pub async fn append(
    tx: &mut Transaction<'_, Postgres>,
    tenant: Uuid,
    loaded: &mut Loaded,
    cmd: &Command,
    processor_ref: Option<&str>,
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

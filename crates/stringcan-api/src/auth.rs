use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::error::ApiError;
use crate::AppState;

pub fn hash_key(key: &str) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}

pub fn generate_api_key() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!("sc_{}", hex::encode(bytes))
}

pub fn admin_matches(expected_hash: &[u8; 32], presented: &str) -> bool {
    expected_hash.ct_eq(&hash_key(presented)).into()
}

fn bearer(parts: &Parts) -> Result<&str, ApiError> {
    parts
        .headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|t| !t.is_empty() && t.len() <= 256)
        .ok_or(ApiError::Unauthorized)
}

/// The tenant is derived from the key; no endpoint takes a tenant id from the
/// caller, so there is no cross-tenant path to forget to check.
pub struct Owner(pub Uuid);

impl FromRequestParts<AppState> for Owner {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = bearer(parts)?;
        let hash = hash_key(token);
        let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM tenants WHERE api_key_hash = $1")
            .bind(hash.as_slice())
            .fetch_optional(&state.pool)
            .await?;
        id.map(Owner).ok_or(ApiError::Unauthorized)
    }
}

pub struct Admin;

impl FromRequestParts<AppState> for Admin {
    type Rejection = ApiError;

    #[allow(clippy::unused_async_trait_impl)] // trait signature is async
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = bearer(parts)?;
        if admin_matches(&state.admin_hash, token) {
            Ok(Admin)
        } else {
            Err(ApiError::Unauthorized)
        }
    }
}

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgSslMode};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DbConfigError {
    #[error("DATABASE_URL is not a valid postgres URL")]
    Invalid,
    #[error("non-local database host {0:?} requires sslmode=verify-full in DATABASE_URL")]
    InsecureRemote(String),
}

fn is_local(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") || host.starts_with('/')
}

/// Remote databases must verify the server certificate and hostname. A weaker
/// mode is rejected rather than silently upgraded, so a misconfigured deploy
/// fails at startup instead of talking to a man in the middle. `allow_insecure`
/// exists for docker-compose, where the database is a sibling container named
/// `db`; it must be an explicit opt-in (`STRINGCAN_ALLOW_INSECURE_DB=1`).
pub fn connect_options(url: &str, allow_insecure: bool) -> Result<PgConnectOptions, DbConfigError> {
    let opts = PgConnectOptions::from_str(url).map_err(|_| DbConfigError::Invalid)?;
    let host = opts.get_host();
    if !allow_insecure
        && !is_local(host)
        && opts.get_socket().is_none()
        && !matches!(opts.get_ssl_mode(), PgSslMode::VerifyFull)
    {
        return Err(DbConfigError::InsecureRemote(host.to_owned()));
    }
    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_hosts_may_skip_tls() {
        for u in [
            "postgres://u:p@localhost/db",
            "postgres://u:p@127.0.0.1:5432/db",
            "postgres://u:p@[::1]/db",
            "postgres:///db?host=/var/run/postgresql",
        ] {
            assert!(connect_options(u, false).is_ok(), "{u}");
        }
    }

    #[test]
    fn remote_hosts_require_verify_full() {
        for mode in [
            "",
            "?sslmode=disable",
            "?sslmode=prefer",
            "?sslmode=require",
            "?sslmode=verify-ca",
        ] {
            let url = format!("postgres://u:p@db.example.com/db{mode}");
            assert_eq!(
                connect_options(&url, false).unwrap_err(),
                DbConfigError::InsecureRemote("db.example.com".into()),
                "{mode}"
            );
        }
        assert!(connect_options(
            "postgres://u:p@db.example.com/db?sslmode=verify-full",
            false
        )
        .is_ok());
    }

    #[test]
    fn explicit_opt_in_allows_a_remote_plaintext_host() {
        assert!(connect_options("postgres://u:p@db/db", true).is_ok());
        assert!(connect_options("postgres://u:p@db/db", false).is_err());
    }

    #[test]
    fn garbage_is_rejected_without_echoing_it() {
        let err = connect_options("not a url with secret", false).unwrap_err();
        assert_eq!(err, DbConfigError::Invalid);
        assert!(!err.to_string().contains("secret"));
    }
}

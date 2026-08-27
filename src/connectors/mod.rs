pub mod csv;
pub mod mysql;
pub mod postgres;
pub mod profiles;
pub mod sqlite;
pub mod sqlserver;

use polars::prelude::DataFrame;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Describes a data source — either a local file or a database connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceConfig {
    File {
        path: String,
    },
    SqlServer {
        host: String,
        /// Defaults to 1433 when omitted.
        port: Option<u16>,
        database: String,
        username: String,
        password: String,
        /// Either a bare table reference ("dbo.customers") or a full SELECT query.
        query: String,
    },
    Postgres {
        host: String,
        /// Defaults to 5432 when omitted.
        port: Option<u16>,
        database: String,
        username: String,
        password: String,
        /// Either a bare table/schema reference ("public.customers") or a full SELECT query.
        query: String,
    },
    Sqlite {
        /// Path to the .db / .sqlite file.
        path: String,
        /// Either a bare table name ("customers") or a full SELECT query.
        query: String,
    },
    Mysql {
        host: String,
        /// Defaults to 3306 when omitted.
        port: Option<u16>,
        database: String,
        username: String,
        password: String,
        /// Either a bare table/schema reference ("customers") or a full SELECT query.
        query: String,
    },
}

impl SourceConfig {
    /// Short human-readable label used in error messages.
    pub fn label(&self) -> String {
        match self {
            SourceConfig::File { path } => path.clone(),
            SourceConfig::SqlServer {
                host,
                port,
                database,
                query,
                ..
            } => {
                format!("{}:{}/{}/{}", host, port.unwrap_or(1433), database, query)
            }
            SourceConfig::Postgres {
                host,
                port,
                database,
                query,
                ..
            } => {
                format!("{}:{}/{}/{}", host, port.unwrap_or(5432), database, query)
            }
            SourceConfig::Sqlite { path, query } => {
                format!("{}/{}", path, query)
            }
            SourceConfig::Mysql {
                host,
                port,
                database,
                query,
                ..
            } => {
                format!("{}:{}/{}/{}", host, port.unwrap_or(3306), database, query)
            }
        }
    }
}

#[derive(Debug)]
pub enum ConnectorError {
    ConnectionFailed(String),
    QueryFailed(String),
    TypeConversion(String),
    Polars(polars::error::PolarsError),
    Io(std::io::Error),
}

impl fmt::Display for ConnectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectorError::ConnectionFailed(m) => write!(f, "Connection failed: {}", m),
            ConnectorError::QueryFailed(m) => write!(f, "Query failed: {}", m),
            ConnectorError::TypeConversion(m) => write!(f, "Type conversion error: {}", m),
            ConnectorError::Polars(e) => write!(f, "Polars error: {}", e),
            ConnectorError::Io(e) => write!(f, "IO error: {}", e),
        }
    }
}

impl std::error::Error for ConnectorError {}

impl From<polars::error::PolarsError> for ConnectorError {
    fn from(e: polars::error::PolarsError) -> Self {
        ConnectorError::Polars(e)
    }
}

impl From<std::io::Error> for ConnectorError {
    fn from(e: std::io::Error) -> Self {
        ConnectorError::Io(e)
    }
}

/// Load a DataFrame from any SourceConfig.
pub async fn load_source(config: &SourceConfig) -> Result<DataFrame, ConnectorError> {
    match config {
        SourceConfig::File { path } => csv::load(path),
        SourceConfig::SqlServer {
            host,
            port,
            database,
            username,
            password,
            query,
        } => {
            sqlserver::load_async(
                host,
                port.unwrap_or(1433),
                database,
                username,
                password,
                query,
            )
            .await
        }
        SourceConfig::Postgres {
            host,
            port,
            database,
            username,
            password,
            query,
        } => {
            postgres::load_async(
                host,
                port.unwrap_or(5432),
                database,
                username,
                password,
                query,
            )
            .await
        }
        SourceConfig::Sqlite { path, query } => {
            let path = path.clone();
            let query = query.clone();
            tokio::task::spawn_blocking(move || sqlite::load(&path, &query))
                .await
                .map_err(|e| ConnectorError::QueryFailed(format!("Task join error: {}", e)))?
        }
        SourceConfig::Mysql {
            host,
            port,
            database,
            username,
            password,
            query,
        } => {
            mysql::load_async(
                host,
                port.unwrap_or(3306),
                database,
                username,
                password,
                query,
            )
            .await
        }
    }
}

/// Read column metadata for a source, if its engine can supply any.
///
/// Never returns an error: every reason metadata might be missing is a variant
/// of [`CatalogAvailability`], so the caller can explain the gap rather than
/// silently reporting less. A connector that cannot do this yet says so by
/// name instead of pretending the source has no catalog.
pub async fn read_catalog(config: &SourceConfig) -> crate::catalog::CatalogAvailability {
    use crate::catalog::CatalogAvailability;

    match config {
        SourceConfig::File { .. } => CatalogAvailability::NotADatabase,
        SourceConfig::Postgres {
            host,
            port,
            database,
            username,
            password,
            query,
        } => {
            postgres::read_catalog(
                host,
                port.unwrap_or(5432),
                database,
                username,
                password,
                query,
            )
            .await
        }
        SourceConfig::Mysql {
            host,
            port,
            database,
            username,
            password,
            query,
        } => {
            mysql::read_catalog(
                host,
                port.unwrap_or(3306),
                database,
                username,
                password,
                query,
            )
            .await
        }
        SourceConfig::SqlServer {
            host,
            port,
            database,
            username,
            password,
            query,
        } => {
            sqlserver::read_catalog(
                host,
                port.unwrap_or(1433),
                database,
                username,
                password,
                query,
            )
            .await
        }
        SourceConfig::Sqlite { path, query } => {
            let path = path.clone();
            let query = query.clone();
            tokio::task::spawn_blocking(move || sqlite::read_catalog(&path, &query))
                .await
                .unwrap_or_else(|e| CatalogAvailability::Failed {
                    reason: format!("task join error: {e}"),
                })
        }
    }
}

/// Parse a source string into a [`SourceConfig`].
///
/// Recognised schemes:
/// - `postgres://user:password@host[:port]/database`
/// - `mysql://user:password@host[:port]/database`
/// - `sqlserver://user:password@host[:port]/database`
/// - `sqlite:///path/to/file.db`
/// - Any other value is treated as a CSV file path.
///
/// For all database sources, `query` must supply a table reference
/// (`schema.table` or a full `SELECT` statement).
pub fn parse_source_uri(source: &str, query: Option<&str>) -> Result<SourceConfig, ConnectorError> {
    if let Some(rest) = source.strip_prefix("postgres://") {
        let (username, password, host, port, database) = parse_db_userinfo_netloc(rest)?;
        let query = require_query(query)?;
        Ok(SourceConfig::Postgres {
            host,
            port,
            database,
            username,
            password,
            query,
        })
    } else if let Some(rest) = source.strip_prefix("mysql://") {
        let (username, password, host, port, database) = parse_db_userinfo_netloc(rest)?;
        let query = require_query(query)?;
        Ok(SourceConfig::Mysql {
            host,
            port,
            database,
            username,
            password,
            query,
        })
    } else if let Some(rest) = source.strip_prefix("sqlserver://") {
        let (username, password, host, port, database) = parse_db_userinfo_netloc(rest)?;
        let query = require_query(query)?;
        Ok(SourceConfig::SqlServer {
            host,
            port,
            database,
            username,
            password,
            query,
        })
    } else if let Some(rest) = source.strip_prefix("sqlite://") {
        let query = require_query(query)?;
        let path = sqlite_path_from_rest(rest);
        Ok(SourceConfig::Sqlite { path, query })
    } else {
        Ok(SourceConfig::File {
            path: source.to_string(),
        })
    }
}

fn require_query(query: Option<&str>) -> Result<String, ConnectorError> {
    query
        .filter(|q| !q.is_empty())
        .map(|q| q.to_string())
        .ok_or_else(|| {
            ConnectorError::ConnectionFailed(
                "A table name or SQL query is required for database sources \
             (use --source-query / --target-query)"
                    .to_string(),
            )
        })
}

/// Parse the `user:password@host[:port]/database` portion of a DB URI.
fn parse_db_userinfo_netloc(
    rest: &str,
) -> Result<(String, String, String, Option<u16>, String), ConnectorError> {
    let err = || {
        ConnectorError::ConnectionFailed(
            "Expected URI format: user:password@host[:port]/database".to_string(),
        )
    };
    let (userinfo, after_at) = rest.split_once('@').ok_or_else(err)?;
    let (username, password) = userinfo.split_once(':').ok_or_else(err)?;
    let (netloc, database) = after_at.split_once('/').ok_or_else(err)?;
    let (host, port) = if let Some((h, p)) = netloc.split_once(':') {
        let port = p
            .parse::<u16>()
            .map_err(|_| ConnectorError::ConnectionFailed(format!("Invalid port number: {}", p)))?;
        (h.to_string(), Some(port))
    } else {
        (netloc.to_string(), None)
    };
    Ok((
        username.to_string(),
        password.to_string(),
        host,
        port,
        database.to_string(),
    ))
}

/// Derive the filesystem path from the portion of a `sqlite://` URI after the scheme.
fn sqlite_path_from_rest(rest: &str) -> String {
    if rest.starts_with('/') {
        #[cfg(unix)]
        {
            return rest.to_string();
        }
        #[cfg(not(unix))]
        {
            return rest.trim_start_matches('/').to_string();
        }
    }
    rest.to_string()
}

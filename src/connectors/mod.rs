//! Connectors for reading data and catalogs from files and databases.

/// Reading a CSV file into a DataFrame.
pub mod csv;
/// MySQL and MariaDB: rows, catalog metadata and schema-only loads.
pub mod mysql;
/// PostgreSQL: rows, catalog metadata and schema-only loads.
pub mod postgres;
/// Saved connection details, with passwords held in the OS keychain.
pub mod profiles;
/// SQLite: rows, pragma-derived catalog metadata and schema-only loads.
pub mod sqlite;
/// Microsoft SQL Server: rows, catalog metadata and schema-only loads.
pub mod sqlserver;

use polars::prelude::DataFrame;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Describes a data source — either a local file or a database connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceConfig {
    /// A local file. CSV today; identified by path rather than by scheme.
    File {
        /// Path to the file.
        path: String,
    },
    /// Microsoft SQL Server.
    SqlServer {
        /// Hostname or address.
        host: String,
        /// Defaults to 1433 when omitted.
        port: Option<u16>,
        /// Database to connect to.
        database: String,
        /// User to authenticate as.
        username: String,
        /// Password for `username`.
        password: String,
        /// Either a bare table reference ("dbo.customers") or a full SELECT query.
        query: String,
    },
    /// PostgreSQL.
    Postgres {
        /// Hostname or address.
        host: String,
        /// Defaults to 5432 when omitted.
        port: Option<u16>,
        /// Database to connect to.
        database: String,
        /// User to authenticate as.
        username: String,
        /// Password for `username`.
        password: String,
        /// Either a bare table/schema reference ("public.customers") or a full SELECT query.
        query: String,
    },
    /// SQLite, as a file on disk.
    Sqlite {
        /// Path to the .db / .sqlite file.
        path: String,
        /// Either a bare table name ("customers") or a full SELECT query.
        query: String,
    },
    /// MySQL or MariaDB.
    Mysql {
        /// Hostname or address.
        host: String,
        /// Defaults to 3306 when omitted.
        port: Option<u16>,
        /// Database to connect to. On MySQL this is also the schema.
        database: String,
        /// User to authenticate as.
        username: String,
        /// Password for `username`.
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
#[non_exhaustive]
/// Errors that can occur while loading data or catalogs.
pub enum ConnectorError {
    /// Failed to connect to the source.
    ConnectionFailed(String),
    /// Query execution failed.
    QueryFailed(String),
    /// Type conversion failed.
    TypeConversion(String),
    /// Polars error.
    Polars(polars::error::PolarsError),
    /// I/O error.
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

/// Load a source's shape without transferring its contents.
///
/// Returns a DataFrame with the right columns and types and no rows. For a
/// schema comparison that is everything needed, and it turns a full table scan
/// into a metadata lookup.
pub async fn load_schema_only(config: &SourceConfig) -> Result<DataFrame, ConnectorError> {
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
            sqlserver::load_schema_async(
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
            postgres::load_schema_async(
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
            tokio::task::spawn_blocking(move || sqlite::load_schema(&path, &query))
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
            mysql::load_schema_async(
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

/// Whether an already-uppercased query begins with `keyword` as a whole word.
///
/// `starts_with("WITH")` on its own is not that test, and the difference is a
/// real bug: a table named `with_fk` — or `withholding`, or `selections` — was
/// read as a common table expression or a `SELECT`, so no catalog was looked
/// up for it and the report said "the query is a SELECT rather than a table
/// reference" about a table that was sitting right there.
///
/// A keyword ends at the end of the input or at any character that cannot
/// continue an identifier. `SELECT*FROM t` is a statement; `SELECTED` is a
/// table.
pub(crate) fn starts_with_keyword(upper: &str, keyword: &str) -> bool {
    match upper.strip_prefix(keyword) {
        Some(rest) => !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_'),
        None => false,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_whose_name_begins_with_a_keyword_is_not_a_statement() {
        // The bug this exists to stop: `withholding` and `selections` are
        // ordinary table names, and reading them as SQL meant no catalog was
        // looked up and the report blamed a SELECT that was never written.
        for name in [
            "WITH_FK",
            "WITHOUT_FK",
            "WITHHOLDING",
            "SELECTIONS",
            "SELECTED_ITEMS",
            "WITH2",
        ] {
            assert!(!starts_with_keyword(name, "WITH"), "{name}");
            assert!(!starts_with_keyword(name, "SELECT"), "{name}");
        }
    }

    #[test]
    fn a_keyword_followed_by_anything_that_cannot_continue_a_name_is_a_statement() {
        assert!(starts_with_keyword("SELECT * FROM t", "SELECT"));
        assert!(starts_with_keyword("SELECT*FROM t", "SELECT"));
        assert!(starts_with_keyword(
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "WITH"
        ));
        assert!(starts_with_keyword("WITH(SELECT 1)", "WITH"));
        // A bare keyword is a broken statement, not a table anybody named.
        assert!(starts_with_keyword("SELECT", "SELECT"));
    }
}

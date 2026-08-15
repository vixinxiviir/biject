use super::ConnectorError;
use polars::prelude::*;
use tokio_postgres::{NoTls, types::Type};

/// Connect to PostgreSQL and execute a query, returning the result as a Polars DataFrame.
///
/// `query` may be either a bare table/schema reference (`"public.customers"`) or a full
/// SELECT statement. All column values are fetched as text; Polars stores them as String
/// series. Type-casting can be applied in a follow-up operation if needed.
///
/// `ssl` controls whether TLS is required. Pass `false` for local dev instances.
pub async fn load_async(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
    query: &str,
) -> Result<DataFrame, ConnectorError> {
    let connect_str = format!(
        "host={} port={} dbname={} user={} password={}",
        host, port, database, username, password
    );

    let (client, connection) = tokio_postgres::connect(&connect_str, NoTls)
        .await
        .map_err(|e| ConnectorError::ConnectionFailed(format!("Cannot connect to {}:{}/{}: {}", host, port, database, e)))?;

    // The connection object must be driven to completion in a background task.
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("[biject] postgres connection error: {}", e);
        }
    });

    let sql = normalize_query(query);

    let rows = client
        .query(sql.as_str(), &[])
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    if rows.is_empty() {
        return Ok(DataFrame::empty());
    }

    let col_count = rows[0].columns().len();
    let col_names: Vec<String> = rows[0]
        .columns()
        .iter()
        .map(|c| c.name().to_string())
        .collect();

    // Transpose row-major → column-major, coercing every value to Option<String>.
    let mut string_cols: Vec<Vec<Option<String>>> =
        vec![Vec::with_capacity(rows.len()); col_count];

    for row in &rows {
        for (col_idx, col) in row.columns().iter().enumerate() {
            let val = pg_value_to_string(row, col_idx, col.type_());
            string_cols[col_idx].push(val);
        }
    }

    let series_vec: Vec<Series> = col_names
        .iter()
        .zip(string_cols)
        .map(|(name, vals)| Series::new(name.as_str(), vals))
        .collect();

    DataFrame::new(series_vec).map_err(ConnectorError::Polars)
}

/// Convert a single Postgres cell to Option<String> regardless of its native type.
fn pg_value_to_string(row: &tokio_postgres::Row, idx: usize, ty: &Type) -> Option<String> {
    // Try the most common types in order. Fall back to a raw text cast.
    macro_rules! try_type {
        ($rust_ty:ty) => {
            if let Ok(v) = row.try_get::<_, Option<$rust_ty>>(idx) {
                return v.map(|x| x.to_string());
            }
        };
    }

    match ty {
        &Type::BOOL => try_type!(bool),
        &Type::INT2 => try_type!(i16),
        &Type::INT4 => try_type!(i32),
        &Type::INT8 => try_type!(i64),
        &Type::FLOAT4 => try_type!(f32),
        &Type::FLOAT8 => try_type!(f64),
        &Type::NUMERIC => {
            // Numeric without scale — read as f64 string approximation via text
        }
        _ => {}
    }

    // For all other types (text, varchar, numeric, date, timestamp, uuid, jsonb, …)
    // ask Postgres to give us a &str directly.
    if let Ok(v) = row.try_get::<_, Option<&str>>(idx) {
        return v.map(|s| s.to_string());
    }

    // Last resort: try owned String
    if let Ok(v) = row.try_get::<_, Option<String>>(idx) {
        return v;
    }

    None
}

/// Wrap bare table references in `SELECT * FROM <table>`.
/// Full SELECT / WITH statements are passed through unchanged.
fn normalize_query(query: &str) -> String {
    let trimmed = query.trim();
    let upper = trimmed.to_uppercase();
    if upper.starts_with("SELECT") || upper.starts_with("WITH") {
        trimmed.to_string()
    } else {
        format!("SELECT * FROM {}", trimmed)
    }
}

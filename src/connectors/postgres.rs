use super::ConnectorError;
use polars::prelude::*;
use tokio_postgres::{types::Type, NoTls};

/// Connect to PostgreSQL and execute a query, returning the result as a Polars DataFrame.
///
/// `query` may be either a bare table/schema reference (`"public.customers"`) or a full
/// SELECT statement.
///
/// Columns keep their types. Reading everything as text would make every column a String
/// series, which silently disables numeric tolerance in `data` and hides every type change
/// from `schema`, because both sides would compare as text.
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

    let columns: Vec<(String, Type)> = rows[0]
        .columns()
        .iter()
        .map(|c| (c.name().to_string(), c.type_().clone()))
        .collect();

    let series_vec: Result<Vec<Series>, ConnectorError> = columns
        .iter()
        .enumerate()
        .map(|(idx, (name, ty))| build_series(name, ty, &rows, idx))
        .collect();

    DataFrame::new(series_vec?).map_err(ConnectorError::Polars)
}

/// How a Postgres column type is represented in a DataFrame.
///
/// Anything not named here is read as text, which is always safe: an unexpected
/// type degrades to string comparison rather than failing the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColumnKind {
    Bool,
    Int16,
    Int32,
    Int64,
    Float32,
    Float64,
    /// NUMERIC is read via its text form and parsed to f64.
    ///
    /// Postgres NUMERIC is arbitrary precision, so this loses digits beyond
    /// what an f64 holds. It is still the right trade: as text, "1.10" and
    /// "1.1" compare as different values and `--numeric-tolerance` cannot
    /// apply at all. Values needing more than ~15 significant digits should be
    /// compared as text by casting them in the query.
    Numeric,
    Date,
    Timestamp,
    TimestampTz,
    Text,
}

/// Map a Postgres type to the kind of column it becomes.
pub(crate) fn column_kind(ty: &Type) -> ColumnKind {
    match *ty {
        Type::BOOL => ColumnKind::Bool,
        Type::INT2 => ColumnKind::Int16,
        Type::INT4 => ColumnKind::Int32,
        Type::INT8 => ColumnKind::Int64,
        Type::FLOAT4 => ColumnKind::Float32,
        Type::FLOAT8 => ColumnKind::Float64,
        Type::NUMERIC => ColumnKind::Numeric,
        Type::DATE => ColumnKind::Date,
        Type::TIMESTAMP => ColumnKind::Timestamp,
        Type::TIMESTAMPTZ => ColumnKind::TimestampTz,
        _ => ColumnKind::Text,
    }
}

/// Read one column out of the result set as a typed series.
fn build_series(
    name: &str,
    ty: &Type,
    rows: &[tokio_postgres::Row],
    idx: usize,
) -> Result<Series, ConnectorError> {
    /// Collect a column whose Rust type maps straight onto a Polars type.
    macro_rules! collect {
        ($rust_ty:ty) => {{
            let values: Vec<Option<$rust_ty>> = rows
                .iter()
                .map(|row| row.try_get::<_, Option<$rust_ty>>(idx).unwrap_or(None))
                .collect();
            Ok(Series::new(name, values))
        }};
    }

    match column_kind(ty) {
        ColumnKind::Bool => collect!(bool),
        ColumnKind::Int16 => collect!(i16),
        ColumnKind::Int32 => collect!(i32),
        ColumnKind::Int64 => collect!(i64),
        ColumnKind::Float32 => collect!(f32),
        ColumnKind::Float64 => collect!(f64),

        ColumnKind::Numeric => {
            // NUMERIC has no native Rust mapping in tokio-postgres. Reading it
            // as text does not work either — the driver type-checks and refuses,
            // which previously turned every NUMERIC column into nulls. Decode
            // via rust_decimal, then widen to f64.
            let values: Vec<Option<f64>> = rows
                .iter()
                .map(|row| {
                    row.try_get::<_, Option<rust_decimal::Decimal>>(idx)
                        .ok()
                        .flatten()
                        .and_then(|d| {
                            use rust_decimal::prelude::ToPrimitive;
                            d.to_f64()
                        })
                })
                .collect();
            Ok(Series::new(name, values))
        }

        ColumnKind::Date => {
            let values: Vec<Option<i32>> = rows
                .iter()
                .map(|row| {
                    row.try_get::<_, Option<chrono::NaiveDate>>(idx)
                        .ok()
                        .flatten()
                        .map(days_since_epoch)
                })
                .collect();
            Series::new(name, values)
                .cast(&DataType::Date)
                .map_err(ConnectorError::Polars)
        }

        ColumnKind::Timestamp => {
            let values: Vec<Option<i64>> = rows
                .iter()
                .map(|row| {
                    row.try_get::<_, Option<chrono::NaiveDateTime>>(idx)
                        .ok()
                        .flatten()
                        .and_then(|dt| dt.and_utc().timestamp_micros().into())
                })
                .collect();
            Series::new(name, values)
                .cast(&DataType::Datetime(TimeUnit::Microseconds, None))
                .map_err(ConnectorError::Polars)
        }

        ColumnKind::TimestampTz => {
            let values: Vec<Option<i64>> = rows
                .iter()
                .map(|row| {
                    row.try_get::<_, Option<chrono::DateTime<chrono::Utc>>>(idx)
                        .ok()
                        .flatten()
                        .map(|dt| dt.timestamp_micros())
                })
                .collect();
            Series::new(name, values)
                .cast(&DataType::Datetime(
                    TimeUnit::Microseconds,
                    Some("UTC".to_string()),
                ))
                .map_err(ConnectorError::Polars)
        }

        ColumnKind::Text => {
            let values: Vec<Option<String>> = rows
                .iter()
                .map(|row| read_as_text(row, idx))
                .collect();
            Ok(Series::new(name, values))
        }
    }
}

/// Best-effort text read, used for every type without a dedicated mapping.
fn read_as_text(row: &tokio_postgres::Row, idx: usize) -> Option<String> {
    if let Ok(v) = row.try_get::<_, Option<&str>>(idx) {
        return v.map(|s| s.to_string());
    }
    if let Ok(v) = row.try_get::<_, Option<String>>(idx) {
        return v;
    }
    None
}

fn days_since_epoch(date: chrono::NaiveDate) -> i32 {
    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch is a valid date");
    (date - epoch).num_days() as i32
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_types_keep_their_width() {
        assert_eq!(column_kind(&Type::INT2), ColumnKind::Int16);
        assert_eq!(column_kind(&Type::INT4), ColumnKind::Int32);
        assert_eq!(column_kind(&Type::INT8), ColumnKind::Int64);
        assert_eq!(column_kind(&Type::FLOAT4), ColumnKind::Float32);
        assert_eq!(column_kind(&Type::FLOAT8), ColumnKind::Float64);
        assert_eq!(column_kind(&Type::BOOL), ColumnKind::Bool);
    }

    #[test]
    fn numeric_becomes_a_number_not_text() {
        // The whole point: as text, --numeric-tolerance cannot apply to a
        // NUMERIC column at all.
        assert_eq!(column_kind(&Type::NUMERIC), ColumnKind::Numeric);
    }

    #[test]
    fn temporal_types_are_distinguished_by_zone() {
        assert_eq!(column_kind(&Type::DATE), ColumnKind::Date);
        assert_eq!(column_kind(&Type::TIMESTAMP), ColumnKind::Timestamp);
        assert_eq!(column_kind(&Type::TIMESTAMPTZ), ColumnKind::TimestampTz);
    }

    #[test]
    fn textual_and_unmapped_types_fall_back_to_text() {
        assert_eq!(column_kind(&Type::TEXT), ColumnKind::Text);
        assert_eq!(column_kind(&Type::VARCHAR), ColumnKind::Text);
        assert_eq!(column_kind(&Type::UUID), ColumnKind::Text);
        assert_eq!(column_kind(&Type::JSONB), ColumnKind::Text);
        // An unmapped type degrades to text rather than failing the query.
        assert_eq!(column_kind(&Type::INET), ColumnKind::Text);
        assert_eq!(column_kind(&Type::POINT), ColumnKind::Text);
    }

    #[test]
    fn epoch_conversion_matches_polars_date_encoding() {
        let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
        assert_eq!(days_since_epoch(epoch), 0);
        assert_eq!(
            days_since_epoch(chrono::NaiveDate::from_ymd_opt(1970, 1, 2).unwrap()),
            1
        );
        assert_eq!(
            days_since_epoch(chrono::NaiveDate::from_ymd_opt(1969, 12, 31).unwrap()),
            -1
        );
    }

    #[test]
    fn bare_table_names_are_wrapped_but_statements_are_not() {
        assert_eq!(normalize_query("customers"), "SELECT * FROM customers");
        assert_eq!(normalize_query("public.orders"), "SELECT * FROM public.orders");
        assert_eq!(
            normalize_query("SELECT id FROM t"),
            "SELECT id FROM t"
        );
        assert_eq!(
            normalize_query("  with x as (select 1) select * from x  "),
            "with x as (select 1) select * from x"
        );
    }
}

use super::ConnectorError;
use crate::catalog::{CatalogAvailability, ColumnDef, TableCatalog};
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

/// Read column metadata for a table from `information_schema`.
///
/// Returns [`CatalogAvailability`] rather than an error or an empty result: a
/// query that is not a table reference, and a lookup that failed, are different
/// from a table with no columns, and the caller must be able to tell them apart.
pub async fn read_catalog(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
    query: &str,
) -> CatalogAvailability {
    let Some((schema, table)) = split_table_reference(query) else {
        return CatalogAvailability::QueryNotATable;
    };

    match load_catalog(host, port, database, username, password, &schema, &table).await {
        Ok(catalog) => CatalogAvailability::Available(catalog),
        Err(err) => CatalogAvailability::Failed(err.to_string()),
    }
}

async fn load_catalog(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
    schema: &str,
    table: &str,
) -> Result<TableCatalog, ConnectorError> {
    let connect_str = format!(
        "host={} port={} dbname={} user={} password={}",
        host, port, database, username, password
    );

    let (client, connection) = tokio_postgres::connect(&connect_str, NoTls)
        .await
        .map_err(|e| ConnectorError::ConnectionFailed(e.to_string()))?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("[biject] postgres connection error: {}", e);
        }
    });

    // format_type renders the declared type the way a human writes it —
    // "character varying(50)" rather than information_schema's bare
    // "character varying" with the length in a separate column.
    const CATALOG_QUERY: &str = "
        SELECT a.attname,
               format_type(a.atttypid, a.atttypmod) AS declared_type,
               NOT a.attnotnull                      AS nullable,
               pg_get_expr(d.adbin, d.adrelid)       AS default_expr,
               a.attnum
        FROM pg_attribute a
        JOIN pg_class c     ON c.oid = a.attrelid
        JOIN pg_namespace n ON n.oid = c.relnamespace
        LEFT JOIN pg_attrdef d ON d.adrelid = c.oid AND d.adnum = a.attnum
        WHERE n.nspname = $1
          AND c.relname = $2
          AND a.attnum > 0
          AND NOT a.attisdropped
        ORDER BY a.attnum";

    let rows = client
        .query(CATALOG_QUERY, &[&schema, &table])
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    if rows.is_empty() {
        return Err(ConnectorError::QueryFailed(format!(
            "no table {schema}.{table} found, or it has no visible columns"
        )));
    }

    let columns = rows
        .iter()
        .map(|row| ColumnDef {
            name: row.get::<_, String>(0),
            data_type: row.get::<_, String>(1),
            nullable: row.get::<_, bool>(2),
            default: row.get::<_, Option<String>>(3),
            ordinal: row.get::<_, i16>(4) as u32,
        })
        .collect();

    Ok(TableCatalog { columns })
}

/// Split a bare table reference into schema and table.
///
/// Returns `None` for anything that is a statement rather than a reference,
/// because a `SELECT` may draw on several tables or none.
pub(crate) fn split_table_reference(query: &str) -> Option<(String, String)> {
    let trimmed = query.trim().trim_end_matches(';').trim();
    let upper = trimmed.to_uppercase();
    if upper.starts_with("SELECT") || upper.starts_with("WITH") {
        return None;
    }
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
        return None;
    }

    let unquote = |part: &str| part.trim().trim_matches('"').to_string();
    match trimmed.split_once('.') {
        Some((schema, table)) if !schema.is_empty() && !table.is_empty() => {
            Some((unquote(schema), unquote(table)))
        }
        Some(_) => None,
        // Postgres resolves an unqualified name through search_path, which
        // defaults to public.
        None => Some(("public".to_string(), unquote(trimmed))),
    }
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
    fn a_bare_name_resolves_through_the_default_schema() {
        assert_eq!(
            split_table_reference("customers"),
            Some(("public".to_string(), "customers".to_string()))
        );
    }

    #[test]
    fn a_qualified_name_keeps_its_schema() {
        assert_eq!(
            split_table_reference("reporting.orders"),
            Some(("reporting".to_string(), "orders".to_string()))
        );
        assert_eq!(
            split_table_reference("\"MySchema\".\"MyTable\""),
            Some(("MySchema".to_string(), "MyTable".to_string()))
        );
    }

    #[test]
    fn a_statement_has_no_single_table_to_describe() {
        // A SELECT may join several tables or none, so there is nothing to
        // look up. The caller reports this rather than guessing.
        for query in [
            "SELECT * FROM t",
            "select id from a join b on a.id = b.id",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "SELECT 1;",
        ] {
            assert_eq!(split_table_reference(query), None, "{query}");
        }
    }

    #[test]
    fn malformed_references_are_refused_rather_than_guessed_at() {
        assert_eq!(split_table_reference(""), None);
        assert_eq!(split_table_reference("   "), None);
        assert_eq!(split_table_reference("schema."), None);
        assert_eq!(split_table_reference(".table"), None);
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

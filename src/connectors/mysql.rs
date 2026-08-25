use super::ConnectorError;
use crate::catalog::{CatalogAvailability, ColumnDef, TableCatalog};
use mysql_async::{consts::ColumnType, prelude::*, Opts, OptsBuilder, Pool, Row, Value};
use polars::prelude::*;

/// Connect to MySQL / MariaDB and execute a query, returning the result as a Polars DataFrame.
///
/// `query` may be a bare table/schema reference (`"mydb.customers"`) or a full SELECT statement.
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
    let opts = OptsBuilder::default()
        .ip_or_hostname(host)
        .tcp_port(port)
        .db_name(Some(database))
        .user(Some(username))
        .pass(Some(password));

    let pool = Pool::new(Opts::from(opts));
    let mut conn = pool
        .get_conn()
        .await
        .map_err(|e| ConnectorError::ConnectionFailed(format!("Cannot connect to {}:{}/{}: {}", host, port, database, e)))?;

    let sql = normalize_query(query);

    let result = conn
        .exec_iter(&sql, ())
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    // Capture the declared type of each column before consuming the result.
    let columns: Vec<(String, ColumnKind)> = result
        .columns_ref()
        .iter()
        .map(|c| {
            let unsigned = c.flags().contains(mysql_async::consts::ColumnFlags::UNSIGNED_FLAG);
            (c.name_str().into_owned(), column_kind(c.column_type(), unsigned))
        })
        .collect();

    let col_count = columns.len();

    let rows: Vec<Row> = result
        .collect_and_drop()
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    // Disconnect cleanly; ignore errors (pool cleanup is best-effort).
    drop(conn);
    pool.disconnect().await.ok();

    // No early return for an empty result. The column types were captured
    // above and still describe the table; discarding them would make an empty
    // table look like a table with no columns.
    let mut cells: Vec<Vec<Value>> = vec![Vec::with_capacity(rows.len()); col_count];
    for mut row in rows {
        for (i, column) in cells.iter_mut().enumerate() {
            column.push(row.take::<Value, _>(i).unwrap_or(Value::NULL));
        }
    }

    let series_vec: Vec<Series> = columns
        .iter()
        .zip(cells)
        .map(|((name, kind), column)| build_series(name, *kind, &column))
        .collect();

    DataFrame::new(series_vec).map_err(ConnectorError::Polars)
}

/// How a MySQL column type is represented in a DataFrame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColumnKind {
    Int8,
    Int16,
    Int32,
    Int64,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    Float32,
    Float64,
    /// DECIMAL arrives as bytes holding its text form, exactly like Postgres
    /// NUMERIC. Left as text it cannot participate in `--numeric-tolerance`.
    Decimal,
    /// DATE has no time component; kept distinct so a DATE to DATETIME change
    /// is visible rather than collapsing into one type.
    Date,
    Datetime,
    Text,
}

/// Map a MySQL column type to the kind of column it becomes.
///
/// Anything unrecognised becomes text, which is always safe: an unexpected type
/// degrades to string comparison rather than failing the query.
pub(crate) fn column_kind(ty: ColumnType, unsigned: bool) -> ColumnKind {
    use ColumnType::*;
    // Integer widths are preserved rather than collapsed to i64, so that an
    // INT to BIGINT change is reported as a type change instead of vanishing.
    match ty {
        MYSQL_TYPE_TINY => pick(unsigned, ColumnKind::Int8, ColumnKind::UInt8),
        MYSQL_TYPE_SHORT => pick(unsigned, ColumnKind::Int16, ColumnKind::UInt16),
        MYSQL_TYPE_LONG | MYSQL_TYPE_INT24 | MYSQL_TYPE_YEAR => {
            pick(unsigned, ColumnKind::Int32, ColumnKind::UInt32)
        }
        MYSQL_TYPE_LONGLONG => pick(unsigned, ColumnKind::Int64, ColumnKind::UInt64),
        MYSQL_TYPE_FLOAT => ColumnKind::Float32,
        MYSQL_TYPE_DOUBLE => ColumnKind::Float64,
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => ColumnKind::Decimal,
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => ColumnKind::Date,
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 | MYSQL_TYPE_TIMESTAMP
        | MYSQL_TYPE_TIMESTAMP2 => ColumnKind::Datetime,
        _ => ColumnKind::Text,
    }
}

fn pick(unsigned: bool, signed: ColumnKind, unsigned_kind: ColumnKind) -> ColumnKind {
    if unsigned {
        unsigned_kind
    } else {
        signed
    }
}

/// Build one typed series from a column's values.
fn build_series(name: &str, kind: ColumnKind, cells: &[Value]) -> Series {
    /// Narrow MySQL's i64/u64 cells into the column's declared width. A value
    /// that will not fit becomes null rather than silently wrapping.
    macro_rules! int_series {
        ($ty:ty) => {{
            let values: Vec<Option<$ty>> = cells
                .iter()
                .map(|v| match v {
                    Value::Int(n) => <$ty>::try_from(*n).ok(),
                    Value::UInt(n) => <$ty>::try_from(*n).ok(),
                    _ => None,
                })
                .collect();
            Series::new(name, values)
        }};
    }

    match kind {
        ColumnKind::Int8 => int_series!(i8),
        ColumnKind::Int16 => int_series!(i16),
        ColumnKind::Int32 => int_series!(i32),
        ColumnKind::Int64 => int_series!(i64),
        ColumnKind::UInt8 => int_series!(u8),
        ColumnKind::UInt16 => int_series!(u16),
        ColumnKind::UInt32 => int_series!(u32),
        ColumnKind::UInt64 => int_series!(u64),
        ColumnKind::Float32 => {
            let values: Vec<Option<f32>> = cells
                .iter()
                .map(|v| match v {
                    Value::Float(f) => Some(*f),
                    Value::Double(f) => Some(*f as f32),
                    _ => None,
                })
                .collect();
            Series::new(name, values)
        }
        ColumnKind::Float64 => {
            let values: Vec<Option<f64>> = cells
                .iter()
                .map(|v| match v {
                    Value::Double(f) => Some(*f),
                    Value::Float(f) => Some(*f as f64),
                    _ => None,
                })
                .collect();
            Series::new(name, values)
        }
        ColumnKind::Decimal => {
            // DECIMAL is delivered as its text form; parse it so tolerance can
            // apply. This widens to f64, so values needing more than ~15
            // significant digits lose precision — cast them in the query if
            // exact comparison matters more than tolerance.
            let values: Vec<Option<f64>> = cells
                .iter()
                .map(|v| match v {
                    Value::Bytes(b) => std::str::from_utf8(b).ok()?.trim().parse::<f64>().ok(),
                    Value::Double(f) => Some(*f),
                    Value::Float(f) => Some(*f as f64),
                    Value::Int(n) => Some(*n as f64),
                    Value::UInt(n) => Some(*n as f64),
                    _ => None,
                })
                .collect();
            Series::new(name, values)
        }
        ColumnKind::Date => {
            let values: Vec<Option<i32>> = cells.iter().map(date_days).collect();
            Series::new(name, values)
                .cast(&DataType::Date)
                .unwrap_or_else(|_| Series::new(name, vec![None::<i32>; cells.len()]))
        }
        ColumnKind::Datetime => {
            let values: Vec<Option<i64>> = cells.iter().map(datetime_micros).collect();
            Series::new(name, values)
                .cast(&DataType::Datetime(TimeUnit::Microseconds, None))
                .unwrap_or_else(|_| Series::new(name, vec![None::<i64>; cells.len()]))
        }
        ColumnKind::Text => {
            let values: Vec<Option<String>> = cells.iter().map(value_to_string).collect();
            Series::new(name, values)
        }
    }
}

/// Microseconds since the Unix epoch, for MySQL's split date representation.
fn datetime_micros(value: &Value) -> Option<i64> {
    let Value::Date(year, month, day, hour, minute, second, micros) = value else {
        return None;
    };
    let date = chrono::NaiveDate::from_ymd_opt(*year as i32, *month as u32, *day as u32)?;
    let time = chrono::NaiveTime::from_hms_micro_opt(
        *hour as u32,
        *minute as u32,
        *second as u32,
        *micros,
    )?;
    Some(date.and_time(time).and_utc().timestamp_micros())
}

/// Days since the Unix epoch, for MySQL DATE columns.
fn date_days(value: &Value) -> Option<i32> {
    let Value::Date(year, month, day, ..) = value else {
        return None;
    };
    let date = chrono::NaiveDate::from_ymd_opt(*year as i32, *month as u32, *day as u32)?;
    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1)?;
    Some((date - epoch).num_days() as i32)
}

/// Best-effort text rendering, used for every type without a dedicated mapping.
fn value_to_string(val: &Value) -> Option<String> {
    match val {
        Value::NULL => None,
        Value::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
        Value::Int(n) => Some(n.to_string()),
        Value::UInt(n) => Some(n.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Double(f) => Some(f.to_string()),
        Value::Date(y, mo, d, h, mi, s, us) => Some(format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:06}",
            y, mo, d, h, mi, s, us
        )),
        Value::Time(neg, days, h, mi, s, us) => {
            let sign = if *neg { "-" } else { "" };
            Some(format!(
                "{}{:02}:{:02}:{:02}.{:06}",
                sign,
                days * 24 + *h as u32,
                mi,
                s,
                us
            ))
        }
    }
}

/// Read column metadata from `information_schema.COLUMNS`.
///
/// Returns [`CatalogAvailability`] rather than an error: a query that is not a
/// table reference and a lookup that failed are different from a table with no
/// columns, and the caller must be able to tell them apart.
pub async fn read_catalog(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
    query: &str,
) -> CatalogAvailability {
    let Some((schema, table)) = split_table_reference(query, database) else {
        return CatalogAvailability::QueryNotATable;
    };

    match load_catalog(host, port, database, username, password, &schema, &table).await {
        Ok(catalog) => CatalogAvailability::Available(catalog),
        Err(err) => CatalogAvailability::Failed { reason: err.to_string() },
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
    let opts = OptsBuilder::default()
        .ip_or_hostname(host)
        .tcp_port(port)
        .db_name(Some(database))
        .user(Some(username))
        .pass(Some(password));

    let pool = Pool::new(Opts::from(opts));
    let mut conn = pool
        .get_conn()
        .await
        .map_err(|e| ConnectorError::ConnectionFailed(e.to_string()))?;

    // COLUMN_TYPE, not DATA_TYPE: the former is the declared type in full,
    // "varchar(50)" or "int unsigned", where the latter is just "varchar" and
    // would erase exactly the distinction this is here to find.
    const CATALOG_QUERY: &str = "
        SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, ORDINAL_POSITION
        FROM information_schema.COLUMNS
        WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
        ORDER BY ORDINAL_POSITION";

    let rows: Vec<(String, String, String, Option<String>, u32)> = conn
        .exec(CATALOG_QUERY, (schema, table))
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    drop(conn);
    pool.disconnect().await.ok();

    if rows.is_empty() {
        return Err(ConnectorError::QueryFailed(format!(
            "no table {schema}.{table} found, or it has no columns"
        )));
    }

    let columns = rows
        .into_iter()
        .map(|(name, data_type, is_nullable, default, ordinal)| ColumnDef {
            name,
            data_type,
            nullable: is_nullable.eq_ignore_ascii_case("YES"),
            default,
            ordinal,
        })
        .collect();

    Ok(TableCatalog { columns })
}

/// Split a table reference into schema and table.
///
/// MySQL calls the namespace a database, so an unqualified name resolves
/// against the one the connection opened rather than a fixed default.
pub(crate) fn split_table_reference(query: &str, database: &str) -> Option<(String, String)> {
    let trimmed = query.trim().trim_end_matches(';').trim();
    let upper = trimmed.to_uppercase();
    if upper.starts_with("SELECT") || upper.starts_with("WITH") {
        return None;
    }
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
        return None;
    }

    let unquote = |part: &str| part.trim().trim_matches('`').trim_matches('"').to_string();
    match trimmed.split_once('.') {
        Some((schema, table)) if !schema.is_empty() && !table.is_empty() => {
            Some((unquote(schema), unquote(table)))
        }
        Some(_) => None,
        None => Some((database.to_string(), unquote(trimmed))),
    }
}

/// Wrap a bare table/schema reference in `SELECT * FROM`.
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
    fn integer_types_respect_the_unsigned_flag() {
        assert_eq!(column_kind(ColumnType::MYSQL_TYPE_LONG, false), ColumnKind::Int32);
        assert_eq!(column_kind(ColumnType::MYSQL_TYPE_LONG, true), ColumnKind::UInt32);
        assert_eq!(column_kind(ColumnType::MYSQL_TYPE_TINY, false), ColumnKind::Int8);
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_LONGLONG, true),
            ColumnKind::UInt64
        );
    }

    #[test]
    fn integer_widths_are_kept_distinct() {
        // Collapsing every integer to i64 would hide an INT to BIGINT change,
        // which is exactly the kind of schema drift this tool exists to report.
        assert_eq!(column_kind(ColumnType::MYSQL_TYPE_TINY, false), ColumnKind::Int8);
        assert_eq!(column_kind(ColumnType::MYSQL_TYPE_SHORT, false), ColumnKind::Int16);
        assert_eq!(column_kind(ColumnType::MYSQL_TYPE_LONG, false), ColumnKind::Int32);
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_LONGLONG, false),
            ColumnKind::Int64
        );
    }

    #[test]
    fn date_is_distinct_from_datetime() {
        // A DATE to DATETIME change is a real schema change and must not be
        // collapsed into a single type.
        assert_eq!(column_kind(ColumnType::MYSQL_TYPE_DATE, false), ColumnKind::Date);
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_DATETIME, false),
            ColumnKind::Datetime
        );
    }

    #[test]
    fn a_value_too_wide_for_its_column_becomes_null_not_a_wrapped_number() {
        // Wrapping 300 into a TINYINT would report a wrong value silently.
        let series = build_series("n", ColumnKind::Int8, &[Value::Int(300)]);
        assert_eq!(series.null_count(), 1);
    }

    #[test]
    fn decimal_becomes_a_number_not_text() {
        // MySQL sends DECIMAL as bytes. Left as text, --numeric-tolerance
        // cannot apply to a money column at all.
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_NEWDECIMAL, false),
            ColumnKind::Decimal
        );
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_DECIMAL, false),
            ColumnKind::Decimal
        );
    }

    #[test]
    fn float_widths_are_preserved() {
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_FLOAT, false),
            ColumnKind::Float32
        );
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_DOUBLE, false),
            ColumnKind::Float64
        );
    }

    #[test]
    fn timestamp_types_become_datetimes() {
        for ty in [
            ColumnType::MYSQL_TYPE_DATETIME,
            ColumnType::MYSQL_TYPE_TIMESTAMP,
        ] {
            assert_eq!(column_kind(ty, false), ColumnKind::Datetime, "{ty:?}");
        }
    }

    #[test]
    fn textual_and_unmapped_types_fall_back_to_text() {
        for ty in [
            ColumnType::MYSQL_TYPE_VARCHAR,
            ColumnType::MYSQL_TYPE_VAR_STRING,
            ColumnType::MYSQL_TYPE_BLOB,
            ColumnType::MYSQL_TYPE_JSON,
            ColumnType::MYSQL_TYPE_GEOMETRY,
            ColumnType::MYSQL_TYPE_TIME,
        ] {
            assert_eq!(column_kind(ty, false), ColumnKind::Text, "{ty:?}");
        }
    }

    #[test]
    fn decimal_text_is_parsed_into_numbers() {
        let cells = vec![
            Value::Bytes(b"100.0040".to_vec()),
            Value::Bytes(b"  2.5  ".to_vec()),
            Value::NULL,
        ];
        let series = build_series("price", ColumnKind::Decimal, &cells);
        assert_eq!(series.dtype(), &DataType::Float64);
        assert_eq!(series.null_count(), 1);
        assert_eq!(series.f64().unwrap().get(0), Some(100.004));
    }

    #[test]
    fn unparseable_decimal_text_becomes_null_not_a_wrong_number() {
        let cells = vec![Value::Bytes(b"not a number".to_vec())];
        let series = build_series("price", ColumnKind::Decimal, &cells);
        assert_eq!(series.null_count(), 1);
    }

    #[test]
    fn integers_keep_their_values() {
        let cells = vec![Value::Int(-5), Value::Int(7), Value::NULL];
        let series = build_series("n", ColumnKind::Int64, &cells);
        assert_eq!(series.dtype(), &DataType::Int64);
        assert_eq!(series.i64().unwrap().get(0), Some(-5));
    }

    #[test]
    fn unsigned_columns_hold_values_above_the_signed_maximum() {
        let cells = vec![Value::UInt(u64::MAX)];
        let series = build_series("n", ColumnKind::UInt64, &cells);
        assert_eq!(series.dtype(), &DataType::UInt64);
        assert_eq!(series.u64().unwrap().get(0), Some(u64::MAX));
    }

    #[test]
    fn dates_convert_to_microseconds_since_the_epoch() {
        assert_eq!(
            datetime_micros(&Value::Date(1970, 1, 1, 0, 0, 0, 0)),
            Some(0)
        );
        assert_eq!(
            datetime_micros(&Value::Date(1970, 1, 1, 0, 0, 1, 0)),
            Some(1_000_000)
        );
        // An impossible date yields None rather than a wrong instant.
        assert_eq!(datetime_micros(&Value::Date(2024, 2, 30, 0, 0, 0, 0)), None);
        assert_eq!(datetime_micros(&Value::Int(5)), None);
    }

    #[test]
    fn an_unqualified_name_resolves_against_the_connected_database() {
        assert_eq!(
            split_table_reference("customers", "shop"),
            Some(("shop".to_string(), "customers".to_string()))
        );
    }

    #[test]
    fn a_qualified_name_overrides_the_connected_database() {
        assert_eq!(
            split_table_reference("archive.customers", "shop"),
            Some(("archive".to_string(), "customers".to_string()))
        );
        assert_eq!(
            split_table_reference("`archive`.`customers`", "shop"),
            Some(("archive".to_string(), "customers".to_string()))
        );
    }

    #[test]
    fn statements_have_no_single_table_to_describe() {
        for query in ["SELECT * FROM t", "WITH x AS (SELECT 1) SELECT * FROM x"] {
            assert_eq!(split_table_reference(query, "shop"), None, "{query}");
        }
    }

    #[test]
    fn bare_table_names_are_wrapped_but_statements_are_not() {
        assert_eq!(normalize_query("customers"), "SELECT * FROM customers");
        assert_eq!(normalize_query("SELECT 1"), "SELECT 1");
    }
}

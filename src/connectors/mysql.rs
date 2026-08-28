use super::{starts_with_keyword, ConnectorError};
use crate::catalog::{
    CatalogAvailability, ColumnDef, Constraint, ConstraintKind, IndexDef, ReferentialAction,
    TableCatalog,
};
use mysql_async::{consts::ColumnType, prelude::*, Opts, OptsBuilder, Pool, Row, Value};
use polars::prelude::*;
use std::collections::BTreeMap;

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
    let mut conn = pool.get_conn().await.map_err(|e| {
        ConnectorError::ConnectionFailed(format!(
            "Cannot connect to {}:{}/{}: {}",
            host, port, database, e
        ))
    })?;

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
            let unsigned = c
                .flags()
                .contains(mysql_async::consts::ColumnFlags::UNSIGNED_FLAG);
            (
                c.name_str().into_owned(),
                column_kind(c.column_type(), unsigned),
            )
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
        MYSQL_TYPE_DATETIME
        | MYSQL_TYPE_DATETIME2
        | MYSQL_TYPE_TIMESTAMP
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
        Ok(Some(catalog)) => CatalogAvailability::Available(catalog),
        Ok(None) => CatalogAvailability::TableNotFound {
            table: format!("{schema}.{table}"),
        },
        Err(err) => CatalogAvailability::Failed {
            reason: err.to_string(),
        },
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
) -> Result<Option<TableCatalog>, ConnectorError> {
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

    if rows.is_empty() {
        drop(conn);
        pool.disconnect().await.ok();
        return Ok(None);
    }

    // Keys and unique constraints. Foreign keys are excluded deliberately: see
    // `catalog::Constraint`.
    const CONSTRAINT_QUERY: &str = "
        SELECT CONSTRAINT_NAME, CONSTRAINT_TYPE
        FROM information_schema.TABLE_CONSTRAINTS
        WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
          AND CONSTRAINT_TYPE IN ('PRIMARY KEY', 'UNIQUE')
        ORDER BY CONSTRAINT_NAME";

    let constraint_rows: Vec<(String, String)> = conn
        .exec(CONSTRAINT_QUERY, (schema, table))
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    const KEY_COLUMN_QUERY: &str = "
        SELECT CONSTRAINT_NAME, COLUMN_NAME
        FROM information_schema.KEY_COLUMN_USAGE
        WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
        ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION";

    let key_column_rows: Vec<(String, String)> = conn
        .exec(KEY_COLUMN_QUERY, (schema, table))
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    let mut key_columns: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for (constraint, column) in key_column_rows {
        key_columns.entry(constraint).or_default().push(column);
    }

    let mut constraints = Vec::new();
    for (name, kind) in constraint_rows {
        // Ordered by ORDINAL_POSITION above, so key order is preserved.
        let columns = key_columns.get(&name).cloned().unwrap_or_default();
        if columns.is_empty() {
            continue;
        }
        constraints.push(if kind == "PRIMARY KEY" {
            Constraint::PrimaryKey { name, columns }
        } else {
            Constraint::Unique { name, columns }
        });
    }

    // CHECK_CONSTRAINTS arrived in MySQL 8.0.16 and MariaDB 10.2. Older servers
    // do not have the view at all, so a failure here means the kind cannot be
    // read rather than that the table has none — recorded, never assumed away.
    //
    // The view carries no table name, hence the join.
    const CHECK_QUERY: &str = "
        SELECT cc.CONSTRAINT_NAME, cc.CHECK_CLAUSE
        FROM information_schema.CHECK_CONSTRAINTS cc
        JOIN information_schema.TABLE_CONSTRAINTS tc
          ON tc.CONSTRAINT_SCHEMA = cc.CONSTRAINT_SCHEMA
         AND tc.CONSTRAINT_NAME = cc.CONSTRAINT_NAME
        WHERE tc.TABLE_SCHEMA = ? AND tc.TABLE_NAME = ?
        ORDER BY cc.CONSTRAINT_NAME";

    let mut unread = Vec::new();
    match conn
        .exec::<(String, String), _, _>(CHECK_QUERY, (schema, table))
        .await
    {
        Ok(rows) => {
            for (name, expression) in rows {
                constraints.push(Constraint::Check { name, expression });
            }
        }
        Err(_) => unread.push(ConstraintKind::Check),
    }

    // Foreign keys: read via KEY_COLUMN_USAGE + REFERENTIAL_CONSTRAINTS.
    // If the query fails, the kind is declared unread rather than silently
    // skipped.
    const FOREIGN_KEY_QUERY: &str = "
        SELECT kcu.CONSTRAINT_NAME,
               kcu.COLUMN_NAME,
               kcu.REFERENCED_TABLE_NAME,
               kcu.REFERENCED_COLUMN_NAME,
               rc.DELETE_RULE,
               rc.UPDATE_RULE
        FROM information_schema.KEY_COLUMN_USAGE kcu
        JOIN information_schema.REFERENTIAL_CONSTRAINTS rc
          ON  rc.CONSTRAINT_SCHEMA = kcu.CONSTRAINT_SCHEMA
         AND rc.CONSTRAINT_NAME   = kcu.CONSTRAINT_NAME
         AND rc.TABLE_NAME        = kcu.TABLE_NAME
        WHERE kcu.TABLE_SCHEMA = ?
          AND kcu.TABLE_NAME   = ?
          AND kcu.REFERENCED_TABLE_NAME IS NOT NULL
        ORDER BY kcu.CONSTRAINT_NAME, kcu.ORDINAL_POSITION";

    match conn
        .exec::<ForeignKeyRow, _, _>(FOREIGN_KEY_QUERY, (schema, table))
        .await
    {
        Ok(rows) => constraints.extend(group_foreign_keys(rows)),
        Err(_) => unread.push(ConstraintKind::ForeignKey),
    }

    // STATISTICS lists the indexes behind primary keys and unique constraints
    // as well as freestanding ones. Those are excluded here because the
    // constraints above already report them; listing both would report every
    // key in the table twice.
    const INDEX_QUERY: &str = "
        SELECT INDEX_NAME, NON_UNIQUE, COLUMN_NAME
        FROM information_schema.STATISTICS
        WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
          AND INDEX_NAME NOT IN (
            SELECT CONSTRAINT_NAME
            FROM information_schema.TABLE_CONSTRAINTS
            WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
          )
        ORDER BY INDEX_NAME, SEQ_IN_INDEX";

    let index_rows: Vec<(String, i64, Option<String>)> = conn
        .exec(INDEX_QUERY, (schema, table, schema, table))
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    drop(conn);
    pool.disconnect().await.ok();

    let mut grouped: std::collections::BTreeMap<String, (bool, Vec<String>)> = Default::default();
    let mut functional = false;
    for (name, non_unique, column) in index_rows {
        // A null column name means a functional index, whose expression lives
        // in a column older servers do not have. Rather than describe such an
        // index with a hole in it, indexes are declared unreadable for this
        // table — blunt, but it never claims an index is something it is not.
        let Some(column) = column else {
            functional = true;
            continue;
        };
        let entry = grouped.entry(name).or_insert((non_unique == 0, Vec::new()));
        entry.1.push(column);
    }

    let indexes: Vec<IndexDef> = if functional {
        unread.push(ConstraintKind::Index);
        Vec::new()
    } else {
        grouped
            .into_iter()
            .map(|(name, (unique, columns))| IndexDef {
                name,
                columns,
                unique,
            })
            .collect()
    };

    let columns = rows
        .into_iter()
        .map(
            |(name, data_type, is_nullable, default, ordinal)| ColumnDef {
                name,
                data_type,
                nullable: is_nullable.eq_ignore_ascii_case("YES"),
                default,
                ordinal,
            },
        )
        .collect();

    Ok(Some(
        TableCatalog::new(columns)
            .with_constraints(constraints)
            .with_indexes(indexes)
            .with_unread(unread),
    ))
}

/// Split a table reference into schema and table.
///
/// MySQL calls the namespace a database, so an unqualified name resolves
/// against the one the connection opened rather than a fixed default.
pub(crate) fn split_table_reference(query: &str, database: &str) -> Option<(String, String)> {
    let trimmed = query.trim().trim_end_matches(';').trim();
    let upper = trimmed.to_uppercase();
    if starts_with_keyword(&upper, "SELECT") || starts_with_keyword(&upper, "WITH") {
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
    if starts_with_keyword(&upper, "SELECT") || starts_with_keyword(&upper, "WITH") {
        trimmed.to_string()
    } else {
        format!("SELECT * FROM {}", trimmed)
    }
}

/// Build a zero-row query for schema-only loading.
fn schema_query(query: &str) -> String {
    let trimmed = query.trim();
    let upper = trimmed.to_uppercase();
    if starts_with_keyword(&upper, "SELECT") || starts_with_keyword(&upper, "WITH") {
        format!("SELECT * FROM ({}) AS biject_schema_probe LIMIT 0", trimmed)
    } else {
        format!("SELECT * FROM {} LIMIT 0", trimmed)
    }
}

/// Load a source's schema without transferring rows.
pub async fn load_schema_async(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
    query: &str,
) -> Result<DataFrame, ConnectorError> {
    let trimmed = query.trim();
    let upper = trimmed.to_uppercase();
    let is_statement = starts_with_keyword(&upper, "SELECT") || starts_with_keyword(&upper, "WITH");
    let schema_sql = schema_query(query);

    match load_async(host, port, database, username, password, &schema_sql).await {
        Ok(df) => Ok(df),
        Err(e) => {
            if is_statement {
                Err(ConnectorError::QueryFailed(format!(
                    "{}\nThis was a schema comparison, which wraps the query to avoid transferring rows.\nA query ending in ORDER BY cannot be wrapped on SQL Server. Remove the ORDER BY — a schema comparison does not depend on row order.",
                    e
                )))
            } else {
                Err(e)
            }
        }
    }
}

/// One row of `FOREIGN_KEY_QUERY`: constraint name, local column, referenced
/// table, referenced column, delete rule, update rule.
type ForeignKeyRow = (String, String, String, String, String, String);

/// A key being assembled: local columns, referenced columns, referenced table,
/// delete action, update action.
type ForeignKeyParts = (Vec<String>, Vec<String>, String, String, String);

/// MySQL reports a foreign key one column at a time, so a two-column key is two
/// rows sharing a constraint name. Gather them back into one constraint.
///
/// Rows must arrive ordered by constraint name then `ORDINAL_POSITION`: key
/// order is not alphabetical order, and a pair assembled in the wrong order is
/// a wrong answer that looks right.
fn group_foreign_keys(rows: Vec<ForeignKeyRow>) -> Vec<Constraint> {
    let mut keys: BTreeMap<String, ForeignKeyParts> = BTreeMap::new();
    for (name, column, referenced_table, referenced_column, delete_rule, update_rule) in rows {
        // The referenced table and the two rules are properties of the key, so
        // every row of one carries the same values. The first row's win.
        let entry = keys.entry(name).or_insert((
            Vec::new(),
            Vec::new(),
            referenced_table,
            delete_rule,
            update_rule,
        ));
        entry.0.push(column);
        entry.1.push(referenced_column);
    }

    keys.into_iter()
        .map(
            |(name, (columns, referenced_columns, referenced_table, on_delete, on_update))| {
                Constraint::ForeignKey {
                    name,
                    columns,
                    referenced_table,
                    referenced_columns,
                    on_delete: referential_action(&on_delete),
                    on_update: referential_action(&on_update),
                }
            },
        )
        .collect()
}

/// One of `REFERENTIAL_CONSTRAINTS.DELETE_RULE` / `UPDATE_RULE`.
///
/// An unrecognised rule is kept verbatim rather than folded into `NoAction`: a
/// new one silently reported as "nothing happens" is a claim about what a real
/// delete does to real rows.
fn referential_action(rule: &str) -> ReferentialAction {
    match rule.trim().to_ascii_uppercase().as_str() {
        "NO ACTION" => ReferentialAction::NoAction,
        "RESTRICT" => ReferentialAction::Restrict,
        "CASCADE" => ReferentialAction::Cascade,
        "SET NULL" => ReferentialAction::SetNull,
        "SET DEFAULT" => ReferentialAction::SetDefault,
        other => ReferentialAction::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_types_respect_the_unsigned_flag() {
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_LONG, false),
            ColumnKind::Int32
        );
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_LONG, true),
            ColumnKind::UInt32
        );
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_TINY, false),
            ColumnKind::Int8
        );
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_LONGLONG, true),
            ColumnKind::UInt64
        );
    }

    #[test]
    fn integer_widths_are_kept_distinct() {
        // Collapsing every integer to i64 would hide an INT to BIGINT change,
        // which is exactly the kind of schema drift this tool exists to report.
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_TINY, false),
            ColumnKind::Int8
        );
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_SHORT, false),
            ColumnKind::Int16
        );
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_LONG, false),
            ColumnKind::Int32
        );
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_LONGLONG, false),
            ColumnKind::Int64
        );
    }

    #[test]
    fn date_is_distinct_from_datetime() {
        // A DATE to DATETIME change is a real schema change and must not be
        // collapsed into a single type.
        assert_eq!(
            column_kind(ColumnType::MYSQL_TYPE_DATE, false),
            ColumnKind::Date
        );
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

    #[test]
    fn a_bare_table_becomes_a_zero_row_select() {
        assert_eq!(schema_query("orders"), "SELECT * FROM orders LIMIT 0");
    }

    #[test]
    fn a_statement_is_wrapped_rather_than_appended_to() {
        let q = "SELECT a FROM t WHERE b > 1";
        let s = schema_query(q);
        assert!(s.starts_with("SELECT * FROM ("));
        assert!(s.ends_with("LIMIT 0"));
        assert!(s.contains(q));
    }

    #[test]
    fn the_subquery_alias_is_present() {
        let s = schema_query("SELECT id FROM t");
        assert!(s.contains("AS biject_schema_probe"), "{}", s);
    }

    // Foreign key grouping tests
    #[test]
    fn a_composite_key_is_one_constraint_rather_than_one_per_column() {
        let rows = vec![
            (
                "fk1".to_string(),
                "a".to_string(),
                "t".to_string(),
                "x".to_string(),
                "CASCADE".to_string(),
                "NO ACTION".to_string(),
            ),
            (
                "fk1".to_string(),
                "b".to_string(),
                "t".to_string(),
                "y".to_string(),
                "CASCADE".to_string(),
                "NO ACTION".to_string(),
            ),
        ];
        let constraints = group_foreign_keys(rows);
        assert_eq!(constraints.len(), 1);
        assert_eq!(constraints[0].columns(), &["a", "b"]);
        assert_eq!(constraints[0].name(), "fk1");
    }

    #[test]
    fn key_order_follows_the_declared_position_not_the_column_name() {
        let rows = vec![
            (
                "fk1".to_string(),
                "b".to_string(),
                "t".to_string(),
                "y".to_string(),
                "CASCADE".to_string(),
                "NO ACTION".to_string(),
            ),
            (
                "fk1".to_string(),
                "a".to_string(),
                "t".to_string(),
                "x".to_string(),
                "CASCADE".to_string(),
                "NO ACTION".to_string(),
            ),
        ];
        // Our grouping preserves input order; the query is ordered by ORDINAL_POSITION,
        // so the test ensures we don't sort by column name.
        let constraints = group_foreign_keys(rows);
        assert_eq!(constraints[0].columns(), &["b", "a"]);
    }

    #[test]
    fn two_keys_on_one_table_do_not_merge() {
        let rows = vec![
            (
                "fk1".to_string(),
                "a".to_string(),
                "t".to_string(),
                "x".to_string(),
                "CASCADE".to_string(),
                "NO ACTION".to_string(),
            ),
            (
                "fk2".to_string(),
                "b".to_string(),
                "t".to_string(),
                "y".to_string(),
                "CASCADE".to_string(),
                "NO ACTION".to_string(),
            ),
        ];
        let constraints = group_foreign_keys(rows);
        assert_eq!(constraints.len(), 2);
    }

    #[test]
    fn an_action_the_engine_spells_differently_is_kept_verbatim() {
        let rows = vec![(
            "fk1".to_string(),
            "a".to_string(),
            "t".to_string(),
            "x".to_string(),
            "UNKNOWN".to_string(),
            "NO ACTION".to_string(),
        )];
        let constraints = group_foreign_keys(rows);
        if let crate::catalog::Constraint::ForeignKey { on_delete, .. } = &constraints[0] {
            assert_eq!(on_delete.to_string(), "UNKNOWN");
        } else {
            panic!("expected foreign key");
        }
    }
}

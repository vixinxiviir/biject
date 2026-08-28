use super::{starts_with_keyword, ConnectorError};
use crate::catalog::{
    CatalogAvailability, ColumnDef, Constraint, IndexDef, ReferentialAction, TableCatalog,
};
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
/// Render a PostgreSQL error in a way that says what went wrong.
///
/// `tokio_postgres::Error`'s `Display` is the bare string "db error". The
/// server's actual message, along with its detail and hint, is only reachable
/// through `as_db_error()`. Passing `Display` through meant every failure this
/// connector reported was indistinguishable from every other: a mistyped table
/// name, a permissions problem and a syntax error all surfaced as
/// "Query failed: db error", which tells a user nothing and cannot be acted on.
fn describe(error: &tokio_postgres::Error) -> String {
    let Some(db) = error.as_db_error() else {
        // A client-side failure — connection, TLS, protocol — whose Display is
        // informative on its own.
        return error.to_string();
    };

    let mut text = db.message().to_string();
    if let Some(detail) = db.detail() {
        text.push_str(" (");
        text.push_str(detail);
        text.push(')');
    }
    if let Some(hint) = db.hint() {
        text.push_str(" — ");
        text.push_str(hint);
    }
    text
}

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
        .map_err(|e| {
            ConnectorError::ConnectionFailed(format!(
                "Cannot connect to {}:{}/{}: {}",
                host, port, database, e
            ))
        })?;

    // The connection object must be driven to completion in a background task.
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("[biject] postgres connection error: {}", e);
        }
    });

    let sql = normalize_query(query);

    // Prepared rather than run directly so the column description is available
    // whether or not any rows come back. Reading the columns from `rows[0]`
    // meant an empty table produced a frame with no columns at all, and a
    // schema comparison against one reported every column as removed.
    let statement = client
        .prepare(sql.as_str())
        .await
        .map_err(|e| ConnectorError::QueryFailed(describe(&e)))?;

    let rows = client
        .query(&statement, &[])
        .await
        .map_err(|e| ConnectorError::QueryFailed(describe(&e)))?;

    let columns: Vec<(String, Type)> = statement
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
            let values: Vec<Option<String>> =
                rows.iter().map(|row| read_as_text(row, idx)).collect();
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
    let connect_str = format!(
        "host={} port={} dbname={} user={} password={}",
        host, port, database, username, password
    );

    let (client, connection) = tokio_postgres::connect(&connect_str, NoTls)
        .await
        .map_err(|e| ConnectorError::ConnectionFailed(describe(&e)))?;
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
        .map_err(|e| ConnectorError::QueryFailed(describe(&e)))?;

    if rows.is_empty() {
        return Ok(None);
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

    // `contype` is Postgres' internal "char" type, which tokio-postgres will
    // not hand back as a String. Cast it in the query rather than decoding a
    // single byte here. The same holds for `confdeltype` and `confupdtype`.
    const CONSTRAINT_QUERY: &str = "
        SELECT con.conname,
               con.contype::text,
               pg_get_expr(con.conbin, con.conrelid) AS check_expr,
               ARRAY(
                 SELECT a.attname
                 FROM unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord)
                 JOIN pg_attribute a
                   ON a.attrelid = con.conrelid AND a.attnum = k.attnum
                 ORDER BY k.ord
               ) AS columns,
               CASE WHEN con.contype = 'f' THEN n2.nspname || '.' || c2.relname ELSE NULL END AS referenced_table,
               CASE WHEN con.contype = 'f' THEN ARRAY(
                 SELECT a2.attname
                 FROM unnest(con.confkey) WITH ORDINALITY AS k(attnum, ord)
                 JOIN pg_attribute a2
                   ON a2.attrelid = con.confrelid AND a2.attnum = k.attnum
                 ORDER BY k.ord
               ) ELSE NULL END AS referenced_columns,
               con.confdeltype::text,
               con.confupdtype::text
        FROM pg_constraint con
        JOIN pg_class c     ON c.oid = con.conrelid
        JOIN pg_namespace n ON n.oid = c.relnamespace
        LEFT JOIN pg_class c2 ON c2.oid = con.confrelid
        LEFT JOIN pg_namespace n2 ON n2.oid = c2.relnamespace
        WHERE n.nspname = $1
          AND c.relname = $2
          AND con.contype IN ('p', 'u', 'c', 'f')
        ORDER BY con.conname";

    let constraint_rows = client
        .query(CONSTRAINT_QUERY, &[&schema, &table])
        .await
        .map_err(|e| ConnectorError::QueryFailed(describe(&e)))?;

    let mut constraints = Vec::with_capacity(constraint_rows.len());
    for row in &constraint_rows {
        let name: String = row.get(0);
        let kind: String = row.get(1);
        let expression: Option<String> = row.get(2);
        let columns: Vec<String> = row.get(3);
        let referenced_table: Option<String> = row.get(4);
        let referenced_columns: Option<Vec<String>> = row.get(5);
        let on_delete_char: Option<String> = row.get(6);
        let on_update_char: Option<String> = row.get(7);

        constraints.push(match kind.as_str() {
            "p" => Constraint::PrimaryKey { name, columns },
            "u" => Constraint::Unique { name, columns },
            "c" => Constraint::Check {
                name,
                // A check with no readable expression is not something to
                // guess at, so it is skipped rather than reported as an empty
                // rule that would compare unequal to everything.
                expression: match expression {
                    Some(expression) => expression,
                    None => continue,
                },
            },
            "f" => {
                // Postgres populates all four of these for every 'f' row. If
                // one is null the query is wrong, and a foreign key with an
                // empty target or an assumed NO ACTION would be a statement
                // about what happens to real rows on a real delete. Fail
                // loudly; the caller turns this into a `Failed` availability
                // that names the reason.
                let (Some(referenced_table), Some(referenced_columns)) =
                    (referenced_table, referenced_columns)
                else {
                    return Err(ConnectorError::QueryFailed(format!(
                        "foreign key `{name}` on {schema}.{table} reports no referenced table"
                    )));
                };
                let (Some(on_delete), Some(on_update)) = (on_delete_char, on_update_char) else {
                    return Err(ConnectorError::QueryFailed(format!(
                        "foreign key `{name}` on {schema}.{table} reports no referential actions"
                    )));
                };
                Constraint::ForeignKey {
                    name,
                    columns,
                    referenced_table,
                    referenced_columns,
                    on_delete: referential_action(&on_delete),
                    on_update: referential_action(&on_update),
                }
            }
            // Anything else is a schema change in Postgres itself rather than in
            // the user's table.
            _ => continue,
        });
    }

    // Indexes that exist only to back a primary key or unique constraint are
    // excluded: the constraint already reports them, and listing both would
    // report every key in the table twice.
    //
    // Columns come from pg_get_indexdef one position at a time, which renders
    // an expression index as its expression — `lower(email::text)` — rather
    // than dropping it. Reading pg_index.indkey directly gives 0 for an
    // expression and would silently lose the column. Note the `+ 1`: indkey is
    // a 0-based int2vector but pg_get_indexdef numbers columns from 1, and
    // position 0 returns the whole CREATE INDEX statement.
    const INDEX_QUERY: &str = "
        SELECT i.relname,
               ix.indisunique,
               ARRAY(
                 SELECT pg_get_indexdef(ix.indexrelid, (k.ord + 1)::integer, true)
                 FROM generate_subscripts(ix.indkey, 1) AS k(ord)
                 ORDER BY k.ord
               ) AS columns
        FROM pg_index ix
        JOIN pg_class i     ON i.oid = ix.indexrelid
        JOIN pg_class c     ON c.oid = ix.indrelid
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = $1
          AND c.relname = $2
          AND NOT EXISTS (
            SELECT 1 FROM pg_constraint con
            WHERE con.conindid = ix.indexrelid
              AND con.contype IN ('p', 'u')
          )
        ORDER BY i.relname";

    let index_rows = client
        .query(INDEX_QUERY, &[&schema, &table])
        .await
        .map_err(|e| ConnectorError::QueryFailed(describe(&e)))?;

    let indexes = index_rows
        .iter()
        .map(|row| IndexDef {
            name: row.get::<_, String>(0),
            unique: row.get::<_, bool>(1),
            columns: row.get::<_, Vec<String>>(2),
        })
        .collect();

    Ok(Some(
        TableCatalog::new(columns)
            .with_constraints(constraints)
            .with_indexes(indexes),
    ))
}

/// One of `pg_constraint.confdeltype` / `confupdtype`, as a modelled action.
///
/// An unrecognised code is kept verbatim rather than folded into `NoAction`.
/// Postgres has added referential actions before and will again, and a new one
/// silently reported as "nothing happens" is a claim about somebody's data.
fn referential_action(code: &str) -> ReferentialAction {
    match code {
        "a" => ReferentialAction::NoAction,
        "r" => ReferentialAction::Restrict,
        "c" => ReferentialAction::Cascade,
        "n" => ReferentialAction::SetNull,
        "d" => ReferentialAction::SetDefault,
        other => ReferentialAction::Other(other.to_string()),
    }
}

/// Split a bare table reference into schema and table.
///
/// Returns `None` for anything that is a statement rather than a reference,
/// because a `SELECT` may draw on several tables or none.
pub(crate) fn split_table_reference(query: &str) -> Option<(String, String)> {
    let trimmed = query.trim().trim_end_matches(';').trim();
    let upper = trimmed.to_uppercase();
    if starts_with_keyword(&upper, "SELECT") || starts_with_keyword(&upper, "WITH") {
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
    fn a_table_named_after_a_keyword_is_still_a_table() {
        // `withholding` begins with WITH and `selections` with SELECT. Reading
        // either as a statement meant no catalog was read for a table that was
        // right there, and the report blamed a SELECT nobody had written.
        assert_eq!(
            split_table_reference("withholding"),
            Some(("public".to_string(), "withholding".to_string()))
        );
        assert_eq!(
            split_table_reference("selections"),
            Some(("public".to_string(), "selections".to_string()))
        );
    }

    #[test]
    fn bare_table_names_are_wrapped_but_statements_are_not() {
        assert_eq!(normalize_query("customers"), "SELECT * FROM customers");
        assert_eq!(
            normalize_query("public.orders"),
            "SELECT * FROM public.orders"
        );
        assert_eq!(normalize_query("SELECT id FROM t"), "SELECT id FROM t");
        assert_eq!(
            normalize_query("  with x as (select 1) select * from x  "),
            "with x as (select 1) select * from x"
        );
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
}

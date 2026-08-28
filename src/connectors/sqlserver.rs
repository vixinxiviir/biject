use super::ConnectorError;
use crate::catalog::{
    CatalogAvailability, ColumnDef, Constraint, ConstraintKind, IndexDef, ReferentialAction,
    TableCatalog,
};
use futures_util::TryStreamExt;
use polars::prelude::*;
use std::collections::BTreeMap;
use tiberius::{AuthMethod, Client, ColumnData, ColumnType, Config, QueryItem, Row};
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncWriteCompatExt;

/// Connect to SQL Server and execute a query, returning the result as a Polars DataFrame.
///
/// `query` may be either a bare table reference (`"dbo.customers"`) or a full SELECT statement.
///
/// Values are read as the types SQL Server sends. Asking for every cell as a string
/// does not work: tiberius refuses the conversion and panics on the first non-string
/// column, which made this connector unusable against any real table.
///
/// Connections use `trust_cert()` - suitable for self-signed/dev certificates.
pub async fn load_async(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
    query: &str,
) -> Result<DataFrame, ConnectorError> {
    let mut config = Config::new();
    config.host(host);
    config.port(port);
    config.database(database);
    config.authentication(AuthMethod::sql_server(username, password));
    // Trust server certificate - change to a TLS-verified config for production.
    config.trust_cert();

    let addr = config.get_addr();
    let tcp = TcpStream::connect(&addr)
        .await
        .map_err(|e| ConnectorError::ConnectionFailed(format!("Cannot reach {}: {}", addr, e)))?;
    tcp.set_nodelay(true)
        .map_err(|e| ConnectorError::ConnectionFailed(e.to_string()))?;

    let mut client = Client::connect(config, tcp.compat_write())
        .await
        .map_err(|e| ConnectorError::ConnectionFailed(e.to_string()))?;

    let sql = normalize_query(query);

    // Walked as a stream rather than collected with `into_first_result`, so the
    // column metadata can be taken from the stream itself. It arrives ahead of
    // the rows, and it arrives even when there are none. Reading the columns
    // from `rows[0]` meant an empty table produced a frame with no columns at
    // all, and a schema comparison against one reported every column as
    // removed rather than reporting no difference.
    let mut stream = client
        .simple_query(sql.as_str())
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    let mut declared: Vec<(String, ColumnType)> = Vec::new();
    let mut rows: Vec<Row> = Vec::new();

    while let Some(item) = stream
        .try_next()
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?
    {
        match item {
            QueryItem::Metadata(metadata) => {
                // Only the first result set is used, which is what
                // `into_first_result` did.
                if metadata.result_index() > 0 {
                    break;
                }
                declared = metadata
                    .columns()
                    .iter()
                    .map(|c| (c.name().to_string(), c.column_type()))
                    .collect();
            }
            QueryItem::Row(row) => rows.push(row),
        }
    }

    let col_count = declared.len();
    if col_count == 0 {
        return Ok(DataFrame::empty());
    }

    let mut cells: Vec<Vec<ColumnData<'static>>> = vec![Vec::with_capacity(rows.len()); col_count];
    for row in rows {
        for (i, data) in row.into_iter().enumerate() {
            if i < col_count {
                cells[i].push(data);
            }
        }
    }

    let series_vec: Vec<Series> = declared
        .iter()
        .zip(cells)
        .map(|((name, declared_type), column)| {
            let kind = kind_of(&column).unwrap_or_else(|| declared_kind(*declared_type));
            build_series(name, kind, &column)
        })
        .collect();

    DataFrame::new(series_vec).map_err(ConnectorError::Polars)
}

/// How a SQL Server column is represented in a DataFrame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColumnKind {
    Bool,
    /// TINYINT is unsigned in SQL Server, 0 to 255.
    UInt8,
    Int16,
    Int32,
    Int64,
    Float32,
    Float64,
    /// DECIMAL and NUMERIC, widened to f64 so tolerance can apply.
    Decimal,
    Date,
    Datetime,
    Text,
}

/// Infer the kind from the first value that is actually present.
///
/// Preferred over the declared type because SQL Server reports nullable columns
/// as the width-erasing `Intn` / `Decimaln` / `Floatn` variants, which cannot
/// tell an INT from a BIGINT. The values themselves carry the real width.
fn kind_of(cells: &[ColumnData<'static>]) -> Option<ColumnKind> {
    cells.iter().find_map(|cell| match cell {
        ColumnData::Bit(Some(_)) => Some(ColumnKind::Bool),
        ColumnData::U8(Some(_)) => Some(ColumnKind::UInt8),
        ColumnData::I16(Some(_)) => Some(ColumnKind::Int16),
        ColumnData::I32(Some(_)) => Some(ColumnKind::Int32),
        ColumnData::I64(Some(_)) => Some(ColumnKind::Int64),
        ColumnData::F32(Some(_)) => Some(ColumnKind::Float32),
        ColumnData::F64(Some(_)) => Some(ColumnKind::Float64),
        ColumnData::Numeric(Some(_)) => Some(ColumnKind::Decimal),
        ColumnData::Date(Some(_)) => Some(ColumnKind::Date),
        ColumnData::DateTime(Some(_))
        | ColumnData::SmallDateTime(Some(_))
        | ColumnData::DateTime2(Some(_))
        | ColumnData::DateTimeOffset(Some(_)) => Some(ColumnKind::Datetime),
        ColumnData::String(Some(_))
        | ColumnData::Guid(Some(_))
        | ColumnData::Binary(Some(_))
        | ColumnData::Xml(Some(_))
        | ColumnData::Time(Some(_)) => Some(ColumnKind::Text),
        // All-null cells carry no evidence; keep looking.
        _ => None,
    })
}

/// Fall back to the declared type for a column with no non-null values.
pub(crate) fn declared_kind(ty: ColumnType) -> ColumnKind {
    match ty {
        ColumnType::Bit | ColumnType::Bitn => ColumnKind::Bool,
        ColumnType::Int1 => ColumnKind::UInt8,
        ColumnType::Int2 => ColumnKind::Int16,
        ColumnType::Int4 => ColumnKind::Int32,
        ColumnType::Int8 => ColumnKind::Int64,
        ColumnType::Float4 => ColumnKind::Float32,
        ColumnType::Float8 => ColumnKind::Float64,
        ColumnType::Decimaln | ColumnType::Numericn | ColumnType::Money | ColumnType::Money4 => {
            ColumnKind::Decimal
        }
        ColumnType::Daten => ColumnKind::Date,
        ColumnType::Datetime
        | ColumnType::Datetime2
        | ColumnType::Datetime4
        | ColumnType::Datetimen
        | ColumnType::DatetimeOffsetn => ColumnKind::Datetime,
        // Intn, Floatn and everything textual or unrecognised. Intn only reaches
        // here for an all-null column, where the width cannot matter.
        _ => ColumnKind::Text,
    }
}

fn build_series(name: &str, kind: ColumnKind, cells: &[ColumnData<'static>]) -> Series {
    match kind {
        ColumnKind::Bool => {
            let values: Vec<Option<bool>> = cells
                .iter()
                .map(|c| match c {
                    ColumnData::Bit(v) => *v,
                    _ => None,
                })
                .collect();
            Series::new(name, values)
        }
        ColumnKind::UInt8 => {
            let values: Vec<Option<u8>> = cells
                .iter()
                .map(|c| match c {
                    ColumnData::U8(v) => *v,
                    _ => None,
                })
                .collect();
            Series::new(name, values)
        }
        ColumnKind::Int16 => {
            let values: Vec<Option<i16>> = cells
                .iter()
                .map(|c| match c {
                    ColumnData::I16(v) => *v,
                    _ => None,
                })
                .collect();
            Series::new(name, values)
        }
        ColumnKind::Int32 => {
            let values: Vec<Option<i32>> = cells
                .iter()
                .map(|c| match c {
                    ColumnData::I32(v) => *v,
                    _ => None,
                })
                .collect();
            Series::new(name, values)
        }
        ColumnKind::Int64 => {
            let values: Vec<Option<i64>> = cells
                .iter()
                .map(|c| match c {
                    ColumnData::I64(v) => *v,
                    _ => None,
                })
                .collect();
            Series::new(name, values)
        }
        ColumnKind::Float32 => {
            let values: Vec<Option<f32>> = cells
                .iter()
                .map(|c| match c {
                    ColumnData::F32(v) => *v,
                    _ => None,
                })
                .collect();
            Series::new(name, values)
        }
        ColumnKind::Float64 => {
            let values: Vec<Option<f64>> = cells
                .iter()
                .map(|c| match c {
                    ColumnData::F64(v) => *v,
                    _ => None,
                })
                .collect();
            Series::new(name, values)
        }
        ColumnKind::Decimal => {
            let values: Vec<Option<f64>> = cells
                .iter()
                .map(|c| match c {
                    ColumnData::Numeric(v) => v.map(f64::from),
                    ColumnData::F64(v) => *v,
                    ColumnData::F32(v) => v.map(|f| f as f64),
                    _ => None,
                })
                .collect();
            Series::new(name, values)
        }
        ColumnKind::Date => {
            let values: Vec<Option<i32>> = cells
                .iter()
                .map(|c| match c {
                    // tiberius Date counts days from 1 January year 1.
                    ColumnData::Date(v) => v.map(|d| d.days() as i32 - DAYS_FROM_YEAR_ONE_TO_EPOCH),
                    _ => None,
                })
                .collect();
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
            let values: Vec<Option<String>> = cells.iter().map(render_text).collect();
            Series::new(name, values)
        }
    }
}

/// Days between 1 January year 1 (tiberius' `Date` epoch) and 1 January 1970.
const DAYS_FROM_YEAR_ONE_TO_EPOCH: i32 = 719_162;

fn datetime_micros(cell: &ColumnData<'static>) -> Option<i64> {
    match cell {
        // DateTime2 stores a date plus a fraction-of-day in nanosecond units.
        ColumnData::DateTime2(Some(dt)) => {
            let days = dt.date().days() as i64 - DAYS_FROM_YEAR_ONE_TO_EPOCH as i64;
            let micros_of_day = time_micros(dt.time().increments(), dt.time().scale());
            Some(days * 86_400_000_000 + micros_of_day)
        }
        ColumnData::DateTimeOffset(Some(dto)) => {
            let dt = dto.datetime2();
            let days = dt.date().days() as i64 - DAYS_FROM_YEAR_ONE_TO_EPOCH as i64;
            let micros_of_day = time_micros(dt.time().increments(), dt.time().scale());
            // offset() is minutes east of UTC; normalise to UTC.
            Some(days * 86_400_000_000 + micros_of_day - dto.offset() as i64 * 60_000_000)
        }
        // DateTime counts whole days from 1 January 1900 plus 1/300 second ticks.
        ColumnData::DateTime(Some(dt)) => {
            let days = dt.days() as i64 - DAYS_FROM_1900_TO_EPOCH as i64;
            let micros = dt.seconds_fragments() as i64 * 1_000_000 / 300;
            Some(days * 86_400_000_000 + micros)
        }
        ColumnData::SmallDateTime(Some(dt)) => {
            let days = dt.days() as i64 - DAYS_FROM_1900_TO_EPOCH as i64;
            Some(days * 86_400_000_000 + dt.seconds_fragments() as i64 * 60_000_000)
        }
        _ => None,
    }
}

/// Days between 1 January 1900 (the `DateTime` epoch) and 1 January 1970.
const DAYS_FROM_1900_TO_EPOCH: i32 = 25_567;

/// Convert a scaled time increment into microseconds since midnight.
fn time_micros(increments: u64, scale: u8) -> i64 {
    // `scale` is the number of decimal digits of a second the increments count.
    let divisor = 10u64.pow(scale as u32);
    ((increments as u128 * 1_000_000) / divisor.max(1) as u128) as i64
}

fn render_text(cell: &ColumnData<'static>) -> Option<String> {
    match cell {
        ColumnData::String(v) => v.as_ref().map(|s| s.to_string()),
        ColumnData::Guid(v) => v.map(|g| g.to_string()),
        ColumnData::Xml(v) => v.as_ref().map(|x| x.to_string()),
        ColumnData::Binary(v) => v.as_ref().map(|b| format!("<binary {} bytes>", b.len())),
        ColumnData::Time(v) => v.map(|t| t.increments().to_string()),
        ColumnData::U8(v) => v.map(|n| n.to_string()),
        ColumnData::I16(v) => v.map(|n| n.to_string()),
        ColumnData::I32(v) => v.map(|n| n.to_string()),
        ColumnData::I64(v) => v.map(|n| n.to_string()),
        ColumnData::F32(v) => v.map(|n| n.to_string()),
        ColumnData::F64(v) => v.map(|n| n.to_string()),
        ColumnData::Bit(v) => v.map(|b| b.to_string()),
        ColumnData::Numeric(v) => v.map(|n| f64::from(n).to_string()),
        _ => None,
    }
}

/// Read column metadata from `INFORMATION_SCHEMA.COLUMNS`.
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
    let mut config = Config::new();
    config.host(host);
    config.port(port);
    config.database(database);
    config.authentication(AuthMethod::sql_server(username, password));
    config.trust_cert();

    let addr = config.get_addr();
    let tcp = TcpStream::connect(&addr)
        .await
        .map_err(|e| ConnectorError::ConnectionFailed(e.to_string()))?;
    tcp.set_nodelay(true)
        .map_err(|e| ConnectorError::ConnectionFailed(e.to_string()))?;
    let mut client = Client::connect(config, tcp.compat_write())
        .await
        .map_err(|e| ConnectorError::ConnectionFailed(e.to_string()))?;

    // Unlike Postgres and MySQL, SQL Server offers no single column holding the
    // declared type in full, so the parts come back separately and are
    // reassembled by render_declared_type.
    const CATALOG_QUERY: &str = "
        SELECT COLUMN_NAME, DATA_TYPE, CHARACTER_MAXIMUM_LENGTH,
               NUMERIC_PRECISION, NUMERIC_SCALE, DATETIME_PRECISION,
               IS_NULLABLE, COLUMN_DEFAULT, ORDINAL_POSITION
        FROM INFORMATION_SCHEMA.COLUMNS
        WHERE TABLE_SCHEMA = @P1 AND TABLE_NAME = @P2
        ORDER BY ORDINAL_POSITION";

    let rows = client
        .query(CATALOG_QUERY, &[&schema, &table])
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?
        .into_first_result()
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    if rows.is_empty() {
        return Ok(None);
    }

    let mut columns = Vec::with_capacity(rows.len());
    for row in rows {
        let name: &str = row.get(0).unwrap_or_default();
        let base_type: &str = row.get(1).unwrap_or_default();
        let char_len: Option<i32> = row.get(2);
        let precision: Option<u8> = row.get(3);
        let scale: Option<i32> = row.get(4);
        let datetime_precision: Option<i16> = row.get(5);
        let is_nullable: &str = row.get(6).unwrap_or("YES");
        let default: Option<&str> = row.get(7);
        let ordinal: i32 = row.get(8).unwrap_or(0);

        columns.push(ColumnDef {
            name: name.to_string(),
            data_type: render_declared_type(
                base_type,
                char_len,
                precision.map(i32::from),
                scale,
                datetime_precision.map(i32::from),
            ),
            nullable: is_nullable.eq_ignore_ascii_case("YES"),
            default: default.map(str::to_string),
            ordinal: ordinal as u32,
        });
    }

    // SQL Server keeps primary keys, unique constraints and plain indexes in
    // one place, distinguished by flags, so a single pass yields all three.
    //
    // `i.type <> 0` skips the heap. `ic.key_ordinal > 0` skips INCLUDE
    // columns, which ride along in the index but are not part of its key —
    // treating them as key columns would make an index compare unequal to the
    // identical index elsewhere.
    const INDEX_QUERY: &str = "
        SELECT i.name, i.is_unique, i.is_primary_key, i.is_unique_constraint,
               c.name, ic.key_ordinal
        FROM sys.indexes i
        JOIN sys.index_columns ic
          ON ic.object_id = i.object_id AND ic.index_id = i.index_id
        JOIN sys.columns c
          ON c.object_id = i.object_id AND c.column_id = ic.column_id
        JOIN sys.objects o ON o.object_id = i.object_id
        JOIN sys.schemas s ON s.schema_id = o.schema_id
        WHERE s.name = @P1 AND o.name = @P2
          AND i.type <> 0
          AND ic.key_ordinal > 0
        ORDER BY i.name, ic.key_ordinal";

    let index_rows = client
        .query(INDEX_QUERY, &[&schema, &table])
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?
        .into_first_result()
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    struct Grouped {
        unique: bool,
        primary_key: bool,
        unique_constraint: bool,
        columns: Vec<String>,
    }

    let mut grouped: std::collections::BTreeMap<String, Grouped> = Default::default();
    for row in index_rows {
        let name: &str = row.get(0).unwrap_or_default();
        let unique: bool = row.get(1).unwrap_or(false);
        let primary_key: bool = row.get(2).unwrap_or(false);
        let unique_constraint: bool = row.get(3).unwrap_or(false);
        let column: &str = row.get(4).unwrap_or_default();

        grouped
            .entry(name.to_string())
            .or_insert(Grouped {
                unique,
                primary_key,
                unique_constraint,
                columns: Vec::new(),
            })
            .columns
            .push(column.to_string());
    }

    let mut constraints = Vec::new();
    let mut indexes = Vec::new();
    for (name, group) in grouped {
        if group.primary_key {
            constraints.push(Constraint::PrimaryKey {
                name,
                columns: group.columns,
            });
        } else if group.unique_constraint {
            constraints.push(Constraint::Unique {
                name,
                columns: group.columns,
            });
        } else {
            // A freestanding index, including a unique one created with
            // CREATE UNIQUE INDEX rather than as a constraint. SQL Server
            // keeps that distinction and so does this.
            indexes.push(IndexDef {
                name,
                columns: group.columns,
                unique: group.unique,
            });
        }
    }

    const CHECK_QUERY: &str = "
        SELECT cc.name, cc.definition
        FROM sys.check_constraints cc
        JOIN sys.objects o ON o.object_id = cc.parent_object_id
        JOIN sys.schemas s ON s.schema_id = o.schema_id
        WHERE s.name = @P1 AND o.name = @P2
        ORDER BY cc.name";

    let check_rows = client
        .query(CHECK_QUERY, &[&schema, &table])
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?
        .into_first_result()
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    for row in check_rows {
        let name: &str = row.get(0).unwrap_or_default();
        let definition: &str = row.get(1).unwrap_or_default();
        constraints.push(Constraint::Check {
            name: name.to_string(),
            expression: definition.to_string(),
        });
    }

    // Foreign keys: read via sys.foreign_keys and sys.foreign_key_columns.
    // If the query fails, the kind is declared unread rather than silently
    // skipped.
    let mut unread = Vec::new();
    let table_identifier = format!("{}.{}", schema, table);
    const FOREIGN_KEY_QUERY: &str = "
        SELECT fk.name,
               OBJECT_SCHEMA_NAME(fk.referenced_object_id) + '.' + OBJECT_NAME(fk.referenced_object_id),
               pc.name,
               rc.name,
               fk.delete_referential_action_desc,
               fk.update_referential_action_desc
        FROM sys.foreign_keys fk
        JOIN sys.foreign_key_columns fkc
          ON fkc.constraint_object_id = fk.object_id
        JOIN sys.columns pc
          ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id
        JOIN sys.columns rc
          ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id
        WHERE fk.parent_object_id = OBJECT_ID(@P1)
        ORDER BY fk.name, fkc.constraint_column_id";

    match client.query(FOREIGN_KEY_QUERY, &[&table_identifier]).await {
        Ok(stream) => {
            let fk_rows = stream
                .into_first_result()
                .await
                .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;
            let rows: Vec<ForeignKeyRow> = fk_rows
                .iter()
                .map(|row| {
                    let cell = |i: usize| -> String {
                        row.get::<&str, _>(i).unwrap_or_default().to_string()
                    };
                    (cell(0), cell(1), cell(2), cell(3), cell(4), cell(5))
                })
                .collect();
            constraints.extend(group_foreign_keys(rows));
        }
        Err(_) => unread.push(ConstraintKind::ForeignKey),
    }

    Ok(Some(
        TableCatalog::new(columns)
            .with_constraints(constraints)
            .with_indexes(indexes)
            .with_unread(unread),
    ))
}

/// Reassemble a declared type from the parts INFORMATION_SCHEMA reports.
///
/// `varchar` with a length of 50 is `varchar(50)`, and a length of -1 is
/// `varchar(max)`, SQL Server's encoding for an unbounded column. Without this
/// every string column would compare as a bare `varchar` and a genuine
/// widening would be invisible, which is the failure this module exists to fix.
pub(crate) fn render_declared_type(
    base: &str,
    char_len: Option<i32>,
    precision: Option<i32>,
    scale: Option<i32>,
    datetime_precision: Option<i32>,
) -> String {
    let base_lower = base.to_ascii_lowercase();

    match base_lower.as_str() {
        "char" | "varchar" | "nchar" | "nvarchar" | "binary" | "varbinary" => match char_len {
            Some(-1) => format!("{base_lower}(max)"),
            Some(len) => format!("{base_lower}({len})"),
            None => base_lower,
        },
        "decimal" | "numeric" => match (precision, scale) {
            (Some(p), Some(s)) => format!("{base_lower}({p},{s})"),
            (Some(p), None) => format!("{base_lower}({p})"),
            _ => base_lower,
        },
        "datetime2" | "time" | "datetimeoffset" => match datetime_precision {
            Some(p) => format!("{base_lower}({p})"),
            None => base_lower,
        },
        _ => base_lower,
    }
}

/// Split a table reference into schema and table.
///
/// SQL Server resolves an unqualified name through the default schema, which
/// is `dbo` unless it has been changed.
pub(crate) fn split_table_reference(query: &str) -> Option<(String, String)> {
    let trimmed = query.trim().trim_end_matches(';').trim();
    let upper = trimmed.to_uppercase();
    if upper.starts_with("SELECT")
        || upper.starts_with("WITH")
        || upper.starts_with("EXEC")
        || upper.starts_with("EXECUTE")
    {
        return None;
    }
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
        return None;
    }

    let unquote = |part: &str| {
        part.trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim_matches('"')
            .to_string()
    };
    match trimmed.split_once('.') {
        Some((schema, table)) if !schema.is_empty() && !table.is_empty() => {
            Some((unquote(schema), unquote(table)))
        }
        Some(_) => None,
        None => Some(("dbo".to_string(), unquote(trimmed))),
    }
}

/// Wrap bare table references in `SELECT * FROM <table>`.
/// Full SELECT / WITH / EXEC statements are passed through unchanged.
fn normalize_query(query: &str) -> String {
    let trimmed = query.trim();
    let upper = trimmed.to_uppercase();
    if upper.starts_with("SELECT")
        || upper.starts_with("WITH")
        || upper.starts_with("EXEC")
        || upper.starts_with("EXECUTE")
    {
        trimmed.to_string()
    } else {
        format!("SELECT * FROM {}", trimmed)
    }
}

/// Build a zero-row query for schema-only loading.
fn schema_query(query: &str) -> String {
    let trimmed = query.trim();
    let upper = trimmed.to_uppercase();
    let is_statement = upper.starts_with("SELECT") || upper.starts_with("WITH");
    if is_statement {
        format!("SELECT TOP 0 * FROM ({}) AS biject_schema_probe", trimmed)
    } else {
        format!("SELECT TOP 0 * FROM {}", trimmed)
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
    let is_statement = upper.starts_with("SELECT") || upper.starts_with("WITH");
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

/// One row of `FOREIGN_KEY_QUERY`: constraint name, referenced table, local
/// column, referenced column, delete action, update action.
type ForeignKeyRow = (String, String, String, String, String, String);

/// A key being assembled: local columns, referenced columns, referenced table,
/// delete action, update action.
type ForeignKeyParts = (Vec<String>, Vec<String>, String, String, String);

/// SQL Server reports a foreign key one column at a time, so a two-column key
/// is two rows sharing a constraint name. Gather them back into one constraint.
///
/// Rows must arrive ordered by name then `constraint_column_id`: key order is
/// not alphabetical order, and a pair assembled in the wrong order is a wrong
/// answer that looks right.
fn group_foreign_keys(rows: Vec<ForeignKeyRow>) -> Vec<Constraint> {
    let mut keys: BTreeMap<String, ForeignKeyParts> = BTreeMap::new();
    for (name, referenced_table, column, referenced_column, on_delete, on_update) in rows {
        // The referenced table and the two actions are properties of the key,
        // so every row of one carries the same values. The first row's win.
        let entry = keys.entry(name).or_insert((
            Vec::new(),
            Vec::new(),
            referenced_table,
            on_delete,
            on_update,
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

/// One of `delete_referential_action_desc` / `update_referential_action_desc`.
///
/// Note the underscores: SQL Server spells these `NO_ACTION` and `SET_NULL`
/// where every other engine here writes `NO ACTION` and `SET NULL`. Copying
/// another connector's table over this one would turn every action into
/// `Other`. SQL Server has no `RESTRICT` at all.
///
/// Anything unrecognised is kept verbatim rather than folded into `NoAction`,
/// which would be a claim about what a real delete does to real rows.
fn referential_action(desc: &str) -> ReferentialAction {
    match desc.trim() {
        "NO_ACTION" => ReferentialAction::NoAction,
        "CASCADE" => ReferentialAction::Cascade,
        "SET_NULL" => ReferentialAction::SetNull,
        "SET_DEFAULT" => ReferentialAction::SetDefault,
        other => ReferentialAction::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_determine_the_kind_so_nullable_widths_survive() {
        // SQL Server reports a nullable INT as Intn, which cannot distinguish
        // INT from BIGINT. The value can.
        let cells = vec![ColumnData::I32(None), ColumnData::I32(Some(5))];
        assert_eq!(kind_of(&cells), Some(ColumnKind::Int32));

        let cells = vec![ColumnData::I64(Some(5))];
        assert_eq!(kind_of(&cells), Some(ColumnKind::Int64));
    }

    #[test]
    fn an_all_null_column_has_no_evidence_and_uses_its_declared_type() {
        let cells = vec![ColumnData::I32(None), ColumnData::I32(None)];
        assert_eq!(kind_of(&cells), None);
        assert_eq!(declared_kind(ColumnType::Int4), ColumnKind::Int32);
        assert_eq!(declared_kind(ColumnType::Bitn), ColumnKind::Bool);
        assert_eq!(declared_kind(ColumnType::Decimaln), ColumnKind::Decimal);
    }

    #[test]
    fn tinyint_is_unsigned_in_sql_server() {
        assert_eq!(declared_kind(ColumnType::Int1), ColumnKind::UInt8);
        let series = build_series("t", ColumnKind::UInt8, &[ColumnData::U8(Some(255))]);
        assert_eq!(series.dtype(), &DataType::UInt8);
        assert_eq!(series.u8().unwrap().get(0), Some(255));
    }

    #[test]
    fn decimals_become_numbers_so_tolerance_can_apply() {
        assert_eq!(
            kind_of(&[ColumnData::Numeric(Some(
                tiberius::numeric::Numeric::new_with_scale(1_000_040, 4)
            ))]),
            Some(ColumnKind::Decimal)
        );
        let series = build_series(
            "price",
            ColumnKind::Decimal,
            &[ColumnData::Numeric(Some(
                tiberius::numeric::Numeric::new_with_scale(1_000_040, 4),
            ))],
        );
        assert_eq!(series.dtype(), &DataType::Float64);
        assert_eq!(series.f64().unwrap().get(0), Some(100.004));
    }

    #[test]
    fn integer_widths_stay_distinct() {
        assert_eq!(declared_kind(ColumnType::Int2), ColumnKind::Int16);
        assert_eq!(declared_kind(ColumnType::Int4), ColumnKind::Int32);
        assert_eq!(declared_kind(ColumnType::Int8), ColumnKind::Int64);
    }

    #[test]
    fn textual_and_unmapped_types_fall_back_to_text() {
        assert_eq!(declared_kind(ColumnType::NVarchar), ColumnKind::Text);
        assert_eq!(declared_kind(ColumnType::BigVarChar), ColumnKind::Text);
        assert_eq!(declared_kind(ColumnType::Guid), ColumnKind::Text);
        assert_eq!(declared_kind(ColumnType::Timen), ColumnKind::Text);
    }

    #[test]
    fn the_epoch_offsets_are_right() {
        // 1970-01-01 is day 719162 counting from 0001-01-01, and day 25567
        // counting from 1900-01-01. Both are load-bearing: an error of one puts
        // every date in the result off by a day.
        let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
        let year_one = chrono::NaiveDate::from_ymd_opt(1, 1, 1).unwrap();
        let nineteen_hundred = chrono::NaiveDate::from_ymd_opt(1900, 1, 1).unwrap();
        assert_eq!(
            (epoch - year_one).num_days() as i32,
            DAYS_FROM_YEAR_ONE_TO_EPOCH
        );
        assert_eq!(
            (epoch - nineteen_hundred).num_days() as i32,
            DAYS_FROM_1900_TO_EPOCH
        );
    }

    #[test]
    fn scaled_time_increments_convert_to_microseconds() {
        // scale 7 means increments of 100 nanoseconds.
        assert_eq!(time_micros(10_000_000, 7), 1_000_000);
        // scale 0 means whole seconds.
        assert_eq!(time_micros(1, 0), 1_000_000);
        assert_eq!(time_micros(0, 7), 0);
    }

    #[test]
    fn declared_types_are_reassembled_with_their_length() {
        // A bare "varchar" would make a widening from 50 to 200 invisible.
        assert_eq!(
            render_declared_type("varchar", Some(50), None, None, None),
            "varchar(50)"
        );
        assert_eq!(
            render_declared_type("nvarchar", Some(200), None, None, None),
            "nvarchar(200)"
        );
    }

    #[test]
    fn minus_one_length_means_max() {
        // SQL Server encodes an unbounded column as a length of -1, which would
        // otherwise render as "varchar(-1)".
        assert_eq!(
            render_declared_type("varchar", Some(-1), None, None, None),
            "varchar(max)"
        );
        assert_eq!(
            render_declared_type("varbinary", Some(-1), None, None, None),
            "varbinary(max)"
        );
    }

    #[test]
    fn decimals_carry_precision_and_scale() {
        assert_eq!(
            render_declared_type("decimal", None, Some(12), Some(4), None),
            "decimal(12,4)"
        );
    }

    #[test]
    fn temporal_types_carry_their_precision() {
        assert_eq!(
            render_declared_type("datetime2", None, None, None, Some(7)),
            "datetime2(7)"
        );
    }

    #[test]
    fn types_without_parameters_stay_bare() {
        assert_eq!(render_declared_type("int", None, None, None, None), "int");
        assert_eq!(
            render_declared_type("BIGINT", None, None, None, None),
            "bigint"
        );
    }

    #[test]
    fn an_unqualified_name_resolves_through_dbo() {
        assert_eq!(
            split_table_reference("customers"),
            Some(("dbo".to_string(), "customers".to_string()))
        );
        assert_eq!(
            split_table_reference("[reporting].[orders]"),
            Some(("reporting".to_string(), "orders".to_string()))
        );
    }

    #[test]
    fn statements_have_no_single_table_to_describe() {
        for query in ["SELECT * FROM t", "EXEC sp_help"] {
            assert_eq!(split_table_reference(query), None, "{query}");
        }
    }

    #[test]
    fn bare_table_names_are_wrapped_but_statements_are_not() {
        assert_eq!(
            normalize_query("dbo.customers"),
            "SELECT * FROM dbo.customers"
        );
        assert_eq!(normalize_query("SELECT 1"), "SELECT 1");
        assert_eq!(normalize_query("EXEC sp_help"), "EXEC sp_help");
    }

    #[test]
    fn a_bare_table_becomes_a_zero_row_select() {
        assert_eq!(schema_query("orders"), "SELECT TOP 0 * FROM orders");
    }

    #[test]
    fn a_statement_is_wrapped_rather_than_appended_to() {
        let q = "SELECT a FROM t WHERE b > 1";
        let s = schema_query(q);
        assert!(s.starts_with("SELECT TOP 0 * FROM ("));
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
                "dbo.t".to_string(),
                "a".to_string(),
                "x".to_string(),
                "NO_ACTION".to_string(),
                "NO_ACTION".to_string(),
            ),
            (
                "fk1".to_string(),
                "dbo.t".to_string(),
                "b".to_string(),
                "y".to_string(),
                "NO_ACTION".to_string(),
                "NO_ACTION".to_string(),
            ),
        ];
        let constraints = group_foreign_keys(rows);
        assert_eq!(constraints.len(), 1);
        assert_eq!(constraints[0].columns(), &["a", "b"]);
    }

    #[test]
    fn key_order_follows_the_declared_position_not_the_column_name() {
        let rows = vec![
            (
                "fk1".to_string(),
                "dbo.t".to_string(),
                "b".to_string(),
                "y".to_string(),
                "NO_ACTION".to_string(),
                "NO_ACTION".to_string(),
            ),
            (
                "fk1".to_string(),
                "dbo.t".to_string(),
                "a".to_string(),
                "x".to_string(),
                "NO_ACTION".to_string(),
                "NO_ACTION".to_string(),
            ),
        ];
        let constraints = group_foreign_keys(rows);
        assert_eq!(constraints[0].columns(), &["b", "a"]);
    }

    #[test]
    fn two_keys_on_one_table_do_not_merge() {
        let rows = vec![
            (
                "fk1".to_string(),
                "dbo.t".to_string(),
                "a".to_string(),
                "x".to_string(),
                "NO_ACTION".to_string(),
                "NO_ACTION".to_string(),
            ),
            (
                "fk2".to_string(),
                "dbo.t".to_string(),
                "b".to_string(),
                "y".to_string(),
                "NO_ACTION".to_string(),
                "NO_ACTION".to_string(),
            ),
        ];
        let constraints = group_foreign_keys(rows);
        assert_eq!(constraints.len(), 2);
    }

    #[test]
    fn an_action_the_engine_spells_differently_is_kept_verbatim() {
        let rows = vec![(
            "fk1".to_string(),
            "dbo.t".to_string(),
            "a".to_string(),
            "x".to_string(),
            "UNKNOWN_ACTION".to_string(),
            "NO_ACTION".to_string(),
        )];
        let constraints = group_foreign_keys(rows);
        if let crate::catalog::Constraint::ForeignKey { on_delete, .. } = &constraints[0] {
            assert_eq!(on_delete.to_string(), "UNKNOWN_ACTION");
        } else {
            panic!("expected foreign key");
        }
    }
}

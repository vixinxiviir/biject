use super::ConnectorError;
use polars::prelude::*;
use tiberius::{AuthMethod, Client, ColumnData, ColumnType, Config};
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

    let rows = client
        .simple_query(sql.as_str())
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?
        .into_first_result()
        .await
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    if rows.is_empty() {
        return Ok(DataFrame::empty());
    }

    // Capture names and declared types before the rows are consumed.
    let declared: Vec<(String, ColumnType)> = rows[0]
        .columns()
        .iter()
        .map(|c| (c.name().to_string(), c.column_type()))
        .collect();
    let col_count = declared.len();

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
    fn bare_table_names_are_wrapped_but_statements_are_not() {
        assert_eq!(normalize_query("dbo.customers"), "SELECT * FROM dbo.customers");
        assert_eq!(normalize_query("SELECT 1"), "SELECT 1");
        assert_eq!(normalize_query("EXEC sp_help"), "EXEC sp_help");
    }
}

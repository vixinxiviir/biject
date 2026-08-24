use super::ConnectorError;
use crate::catalog::{CatalogAvailability, ColumnDef, TableCatalog};
use polars::prelude::*;
use rusqlite::{types::ValueRef, Connection};

/// Open a SQLite database file and execute a query, returning the result as a Polars DataFrame.
///
/// `query` may be a bare table name (`"customers"`) or a full SELECT statement.
///
/// Columns keep their types. Reading everything as text would make every column a
/// String series, which silently disables numeric tolerance in `data` and hides every
/// type change from `schema`, because both sides would compare as text.
pub fn load(path: &str, query: &str) -> Result<DataFrame, ConnectorError> {
    let conn = Connection::open(path)
        .map_err(|e| ConnectorError::ConnectionFailed(format!("Cannot open '{}': {}", path, e)))?;

    let sql = normalize_query(query);

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    let col_count = stmt.column_count();
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

    // SQLite reports the declared type of each result column when it maps back
    // to a table column. Expressions and literals have none. Scoped so the
    // borrow ends before the statement is queried.
    let affinities: Vec<Affinity> = {
        let columns = stmt.columns();
        columns
            .iter()
            .map(|column| affinity_of(column.decl_type()))
            .collect()
    };

    let mut cells: Vec<Vec<Cell>> = vec![Vec::new(); col_count];

    let mut rows = stmt
        .query([])
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    while let Some(row) = rows
        .next()
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?
    {
        for (i, column) in cells.iter_mut().enumerate() {
            let cell = match row
                .get_ref(i)
                .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?
            {
                ValueRef::Null => Cell::Null,
                ValueRef::Integer(n) => Cell::Int(n),
                ValueRef::Real(f) => Cell::Real(f),
                ValueRef::Text(s) => Cell::Text(String::from_utf8_lossy(s).into_owned()),
                ValueRef::Blob(b) => Cell::Text(format!("<blob {} bytes>", b.len())),
            };
            column.push(cell);
        }
    }

    if col_count == 0 || cells[0].is_empty() {
        return Ok(DataFrame::empty());
    }

    let series_vec: Vec<Series> = col_names
        .iter()
        .zip(affinities)
        .zip(cells)
        .map(|((name, affinity), column)| series_from_cells(name, affinity, &column))
        .collect();

    DataFrame::new(series_vec).map_err(ConnectorError::Polars)
}

/// One value as SQLite actually stored it.
///
/// SQLite is dynamically typed: a column's declared type is an affinity, not a
/// constraint, so any row may hold any storage class.
#[derive(Debug, Clone, PartialEq)]
enum Cell {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
}

/// SQLite type affinity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Affinity {
    Integer,
    Real,
    Numeric,
    Text,
    Blob,
}

/// Apply SQLite's documented rules for determining column affinity.
///
/// The order of these checks is part of the algorithm — "INT" wins over
/// everything, which is why `POINT` has integer affinity, and why the rules
/// cannot be reordered for readability.
pub(crate) fn affinity_of(decltype: Option<&str>) -> Affinity {
    let Some(declared) = decltype else {
        return Affinity::Blob;
    };
    let upper = declared.to_ascii_uppercase();

    if upper.contains("INT") {
        Affinity::Integer
    } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
        Affinity::Text
    } else if upper.contains("BLOB") || upper.is_empty() {
        Affinity::Blob
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        Affinity::Real
    } else {
        Affinity::Numeric
    }
}

/// Choose a series type from the declared affinity, widened to fit the values
/// actually present.
///
/// Affinity alone is not enough: it is a hint, and SQLite will happily store a
/// string in an INTEGER column. Values alone are not enough either, because an
/// all-null column carries no evidence. Using affinity as the starting point
/// and widening when the data demands it keeps both cases correct without ever
/// discarding a value.
fn series_from_cells(name: &str, affinity: Affinity, cells: &[Cell]) -> Series {
    let has_text = cells.iter().any(|c| matches!(c, Cell::Text(_)));
    let has_real = cells.iter().any(|c| matches!(c, Cell::Real(_)));

    let numeric_intent = matches!(
        affinity,
        Affinity::Integer | Affinity::Real | Affinity::Numeric
    );

    // Any text in a column forces text, whatever was declared — coercing it
    // away would silently drop the value.
    if has_text || !numeric_intent {
        let values: Vec<Option<String>> = cells
            .iter()
            .map(|cell| match cell {
                Cell::Null => None,
                Cell::Int(n) => Some(n.to_string()),
                Cell::Real(f) => Some(f.to_string()),
                Cell::Text(s) => Some(s.clone()),
            })
            .collect();
        return Series::new(name, values);
    }

    // Integer affinity holds only while every value is an integer.
    if affinity == Affinity::Integer && !has_real {
        let values: Vec<Option<i64>> = cells
            .iter()
            .map(|cell| match cell {
                Cell::Int(n) => Some(*n),
                _ => None,
            })
            .collect();
        return Series::new(name, values);
    }

    let values: Vec<Option<f64>> = cells
        .iter()
        .map(|cell| match cell {
            Cell::Int(n) => Some(*n as f64),
            Cell::Real(f) => Some(*f),
            _ => None,
        })
        .collect();
    Series::new(name, values)
}

/// Read column metadata for a table via `pragma_table_info`.
///
/// Returns [`CatalogAvailability`] rather than an error: a query that is not a
/// table reference and a lookup that failed are different from a table with no
/// columns, and the caller must be able to tell them apart.
pub fn read_catalog(path: &str, query: &str) -> CatalogAvailability {
    let Some((schema, table)) = split_table_reference(query) else {
        return CatalogAvailability::QueryNotATable;
    };

    match load_catalog(path, schema.as_deref(), &table) {
        Ok(catalog) => CatalogAvailability::Available(catalog),
        Err(err) => CatalogAvailability::Failed { reason: err.to_string() },
    }
}

fn load_catalog(
    path: &str,
    schema: Option<&str>,
    table: &str,
) -> Result<TableCatalog, ConnectorError> {
    let conn = Connection::open(path)
        .map_err(|e| ConnectorError::ConnectionFailed(format!("Cannot open '{}': {}", path, e)))?;

    // The table-valued form takes bound parameters, unlike `PRAGMA x(y)`, so
    // the table name never has to be interpolated into SQL.
    let sql = if schema.is_some() {
        "SELECT cid, name, type, \"notnull\", dflt_value          FROM pragma_table_info(?1, ?2) ORDER BY cid"
    } else {
        "SELECT cid, name, type, \"notnull\", dflt_value          FROM pragma_table_info(?1) ORDER BY cid"
    };

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    let map_row = |row: &rusqlite::Row| -> rusqlite::Result<ColumnDef> {
        let cid: i64 = row.get(0)?;
        let declared: String = row.get(2)?;
        let not_null: i64 = row.get(3)?;
        Ok(ColumnDef {
            name: row.get(1)?,
            // SQLite keeps the declared type verbatim, including a length, and
            // will happily store anything regardless. It is still what the
            // schema says, which is what a comparison is about.
            data_type: declared,
            nullable: not_null == 0,
            default: row.get::<_, Option<String>>(4)?,
            // cid is 0-based; every other connector reports 1-based, and a
            // mismatch would show as a spurious reordering on every column.
            ordinal: (cid + 1) as u32,
        })
    };

    let columns: Vec<ColumnDef> = match schema {
        Some(schema) => stmt
            .query_map(rusqlite::params![table, schema], map_row)
            .and_then(|rows| rows.collect())
            .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?,
        None => stmt
            .query_map(rusqlite::params![table], map_row)
            .and_then(|rows| rows.collect())
            .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?,
    };

    if columns.is_empty() {
        return Err(ConnectorError::QueryFailed(format!(
            "no table {table} found, or it has no columns"
        )));
    }

    Ok(TableCatalog { columns })
}

/// Split a table reference into an optional schema and a table name.
///
/// SQLite qualifies by attached database rather than schema, so an unqualified
/// name is left unqualified rather than defaulting to `main` — that is what
/// `pragma_table_info` does on its own, across all attached databases.
pub(crate) fn split_table_reference(query: &str) -> Option<(Option<String>, String)> {
    let trimmed = query.trim().trim_end_matches(';').trim();
    let upper = trimmed.to_uppercase();
    if upper.starts_with("SELECT") || upper.starts_with("WITH") {
        return None;
    }
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
        return None;
    }

    let unquote = |part: &str| part.trim().trim_matches('"').trim_matches('`').to_string();
    match trimmed.split_once('.') {
        Some((schema, table)) if !schema.is_empty() && !table.is_empty() => {
            Some((Some(unquote(schema)), unquote(table)))
        }
        Some(_) => None,
        None => Some((None, unquote(trimmed))),
    }
}

/// Wrap a bare table name in `SELECT * FROM <table>`.
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
    fn affinity_follows_sqlites_own_rules() {
        assert_eq!(affinity_of(Some("INTEGER")), Affinity::Integer);
        assert_eq!(affinity_of(Some("BIGINT")), Affinity::Integer);
        assert_eq!(affinity_of(Some("VARCHAR(20)")), Affinity::Text);
        assert_eq!(affinity_of(Some("TEXT")), Affinity::Text);
        assert_eq!(affinity_of(Some("BLOB")), Affinity::Blob);
        assert_eq!(affinity_of(Some("REAL")), Affinity::Real);
        assert_eq!(affinity_of(Some("DOUBLE PRECISION")), Affinity::Real);
        assert_eq!(affinity_of(Some("FLOAT")), Affinity::Real);
        assert_eq!(affinity_of(Some("DECIMAL(10,5)")), Affinity::Numeric);
        assert_eq!(affinity_of(Some("DATE")), Affinity::Numeric);
    }

    #[test]
    fn int_wins_over_later_rules_as_the_algorithm_requires() {
        // "POINT" contains INT, so SQLite gives it integer affinity. Surprising,
        // but it is the documented behaviour and reordering the checks for
        // tidiness would change results.
        assert_eq!(affinity_of(Some("POINT")), Affinity::Integer);
        // Likewise a declared type containing both INT and CHAR.
        assert_eq!(affinity_of(Some("INTCHAR")), Affinity::Integer);
    }

    #[test]
    fn an_expression_column_has_no_declared_type() {
        assert_eq!(affinity_of(None), Affinity::Blob);
    }

    #[test]
    fn integer_columns_stay_integers() {
        let cells = vec![Cell::Int(1), Cell::Null, Cell::Int(3)];
        let series = series_from_cells("n", Affinity::Integer, &cells);
        assert_eq!(series.dtype(), &DataType::Int64);
        assert_eq!(series.null_count(), 1);
    }

    #[test]
    fn real_columns_become_floats_so_tolerance_can_apply() {
        let cells = vec![Cell::Real(1.5), Cell::Real(2.5)];
        let series = series_from_cells("x", Affinity::Real, &cells);
        assert_eq!(series.dtype(), &DataType::Float64);
    }

    #[test]
    fn an_integer_column_holding_a_real_widens_rather_than_truncating() {
        // Truncating 1.5 to 1 would silently change the data.
        let cells = vec![Cell::Int(1), Cell::Real(1.5)];
        let series = series_from_cells("n", Affinity::Integer, &cells);
        assert_eq!(series.dtype(), &DataType::Float64);
    }

    #[test]
    fn any_text_forces_the_whole_column_to_text() {
        // SQLite permits a string in a numeric column. Dropping it to keep the
        // column numeric would lose the value entirely.
        let cells = vec![Cell::Int(1), Cell::Text("n/a".to_string())];
        let series = series_from_cells("n", Affinity::Integer, &cells);
        assert_eq!(series.dtype(), &DataType::String);
        assert_eq!(series.null_count(), 0, "no value is discarded");
    }

    #[test]
    fn numeric_affinity_is_treated_as_a_number() {
        let cells = vec![Cell::Int(1), Cell::Real(2.5)];
        let series = series_from_cells("d", Affinity::Numeric, &cells);
        assert_eq!(series.dtype(), &DataType::Float64);
    }

    #[test]
    fn text_and_blob_affinity_stay_text() {
        let cells = vec![Cell::Text("a".to_string())];
        assert_eq!(
            series_from_cells("s", Affinity::Text, &cells).dtype(),
            &DataType::String
        );
        assert_eq!(
            series_from_cells("b", Affinity::Blob, &cells).dtype(),
            &DataType::String
        );
    }

    #[test]
    fn an_all_null_column_falls_back_to_its_declared_affinity() {
        let cells = vec![Cell::Null, Cell::Null];
        assert_eq!(
            series_from_cells("n", Affinity::Integer, &cells).dtype(),
            &DataType::Int64
        );
        assert_eq!(
            series_from_cells("s", Affinity::Text, &cells).dtype(),
            &DataType::String
        );
    }

    #[test]
    fn an_unqualified_reference_stays_unqualified() {
        assert_eq!(
            split_table_reference("customers"),
            Some((None, "customers".to_string()))
        );
    }

    #[test]
    fn an_attached_database_qualifier_is_kept() {
        assert_eq!(
            split_table_reference("archive.customers"),
            Some((Some("archive".to_string()), "customers".to_string()))
        );
    }

    #[test]
    fn statements_have_no_single_table_to_describe() {
        for query in ["SELECT * FROM t", "WITH x AS (SELECT 1) SELECT * FROM x", "SELECT 1;"] {
            assert_eq!(split_table_reference(query), None, "{query}");
        }
    }

    #[test]
    fn malformed_references_are_refused() {
        assert_eq!(split_table_reference(""), None);
        assert_eq!(split_table_reference("schema."), None);
        assert_eq!(split_table_reference(".table"), None);
    }

    #[test]
    fn bare_table_names_are_wrapped_but_statements_are_not() {
        assert_eq!(normalize_query("customers"), "SELECT * FROM customers");
        assert_eq!(normalize_query("SELECT 1"), "SELECT 1");
    }
}

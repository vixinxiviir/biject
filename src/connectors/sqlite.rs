use super::ConnectorError;
use crate::catalog::{
    CatalogAvailability, ColumnDef, Constraint, ConstraintKind, IndexDef, TableCatalog,
};
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

    // Only a genuinely column-less statement yields an empty frame. Having no
    // rows is not the same thing, and treating it as such made an empty table
    // compare as though every column had been dropped.
    if col_count == 0 {
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
        Ok(Some(catalog)) => CatalogAvailability::Available(catalog),
        Ok(None) => CatalogAvailability::TableNotFound {
            table: table.clone(),
        },
        Err(err) => CatalogAvailability::Failed {
            reason: err.to_string(),
        },
    }
}

fn load_catalog(
    path: &str,
    schema: Option<&str>,
    table: &str,
) -> Result<Option<TableCatalog>, ConnectorError> {
    let conn = Connection::open(path)
        .map_err(|e| ConnectorError::ConnectionFailed(format!("Cannot open '{}': {}", path, e)))?;

    // The table-valued form takes bound parameters, unlike `PRAGMA x(y)`, so
    // the table name never has to be interpolated into SQL.
    let sql = if schema.is_some() {
        "SELECT cid, name, type, \"notnull\", dflt_value, pk          FROM pragma_table_info(?1, ?2) ORDER BY cid"
    } else {
        "SELECT cid, name, type, \"notnull\", dflt_value, pk          FROM pragma_table_info(?1) ORDER BY cid"
    };

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    // The second element is the column's 1-based position in the primary key,
    // or 0 when it is not part of one.
    let map_row = |row: &rusqlite::Row| -> rusqlite::Result<(ColumnDef, i64)> {
        let cid: i64 = row.get(0)?;
        let declared: String = row.get(2)?;
        let not_null: i64 = row.get(3)?;
        let key_position: i64 = row.get(5)?;
        Ok((
            ColumnDef {
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
            },
            key_position,
        ))
    };

    let with_key_positions: Vec<(ColumnDef, i64)> = match schema {
        Some(schema) => stmt
            .query_map(rusqlite::params![table, schema], map_row)
            .and_then(|rows| rows.collect())
            .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?,
        None => stmt
            .query_map(rusqlite::params![table], map_row)
            .and_then(|rows| rows.collect())
            .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?,
    };

    if with_key_positions.is_empty() {
        return Ok(None);
    }

    // The primary key comes from `table_info`, not from `index_list`. A rowid
    // alias — `id INTEGER PRIMARY KEY`, the commonest primary key in SQLite —
    // carries a pk position in `table_info` but produces no index whatsoever,
    // so reading the index list alone would miss it entirely.
    let mut key_columns: Vec<(i64, String)> = with_key_positions
        .iter()
        .filter(|(_, position)| *position > 0)
        .map(|(column, position)| (*position, column.name.clone()))
        .collect();
    key_columns.sort_by_key(|(position, _)| *position);

    let mut constraints = Vec::new();
    if !key_columns.is_empty() {
        constraints.push(Constraint::PrimaryKey {
            // SQLite does not name an implicit primary key. Invented here so
            // there is something to render in DDL; comparison matches on
            // columns, never on the name.
            name: format!("{table}_pkey"),
            columns: key_columns.into_iter().map(|(_, name)| name).collect(),
        });
    }

    let columns: Vec<ColumnDef> = with_key_positions
        .into_iter()
        .map(|(column, _)| column)
        .collect();

    // `origin` says where an index came from: 'c' for a CREATE INDEX, 'u' for
    // a UNIQUE constraint, 'pk' for a primary key. The 'pk' entries are
    // dropped because `table_info` already reported that key, and listing both
    // would report every composite primary key twice.
    let list_sql = if schema.is_some() {
        "SELECT name, \"unique\", origin FROM pragma_index_list(?1, ?2)"
    } else {
        "SELECT name, \"unique\", origin FROM pragma_index_list(?1)"
    };

    let mut list_stmt = conn
        .prepare(list_sql)
        .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

    let map_index = |row: &rusqlite::Row| -> rusqlite::Result<(String, bool, String)> {
        let unique: i64 = row.get(1)?;
        Ok((row.get(0)?, unique == 1, row.get(2)?))
    };

    let listed: Vec<(String, bool, String)> = match schema {
        Some(schema) => list_stmt
            .query_map(rusqlite::params![table, schema], map_index)
            .and_then(|rows| rows.collect())
            .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?,
        None => list_stmt
            .query_map(rusqlite::params![table], map_index)
            .and_then(|rows| rows.collect())
            .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?,
    };
    drop(list_stmt);

    let mut indexes = Vec::new();
    for (name, unique, origin) in listed {
        if origin == "pk" {
            continue;
        }

        let info_sql = if schema.is_some() {
            "SELECT name FROM pragma_index_info(?1, ?2) ORDER BY seqno"
        } else {
            "SELECT name FROM pragma_index_info(?1) ORDER BY seqno"
        };
        let mut info_stmt = conn
            .prepare(info_sql)
            .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?;

        let map_name = |row: &rusqlite::Row| -> rusqlite::Result<Option<String>> { row.get(0) };
        let index_columns: Vec<Option<String>> = match schema {
            Some(schema) => info_stmt
                .query_map(rusqlite::params![name, schema], map_name)
                .and_then(|rows| rows.collect())
                .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?,
            None => info_stmt
                .query_map(rusqlite::params![name], map_name)
                .and_then(|rows| rows.collect())
                .map_err(|e| ConnectorError::QueryFailed(e.to_string()))?,
        };

        // A null name means the index is over an expression rather than a
        // column. SQLite does not expose the expression through this pragma,
        // so the index cannot be described and is skipped rather than
        // reported with a hole in it.
        let Some(index_columns) = index_columns.into_iter().collect::<Option<Vec<String>>>() else {
            continue;
        };

        if origin == "u" {
            constraints.push(Constraint::Unique {
                name,
                columns: index_columns,
            });
        } else {
            indexes.push(IndexDef {
                name,
                columns: index_columns,
                unique,
            });
        }
    }

    // CHECK constraints are the one kind SQLite will not report. They live only
    // in the original CREATE TABLE text in `sqlite_master`, and recovering them
    // means parsing SQL. Declared unread so their absence is never read as
    // evidence that the table has none.
    Ok(Some(
        TableCatalog::new(columns)
            .with_constraints(constraints)
            .with_indexes(indexes)
            .with_unread(vec![ConstraintKind::Check]),
    ))
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

    /// A real SQLite file, since these paths are about what the driver does.
    fn scratch_db(name: &str, ddl: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("biject_sqlite_{name}.sqlite"));
        let _ = std::fs::remove_file(&path);
        let connection = rusqlite::Connection::open(&path).expect("create the database");
        connection.execute_batch(ddl).expect("run the fixture DDL");
        path
    }

    #[test]
    fn a_table_with_no_rows_still_reports_its_columns() {
        // Having no rows is not the same as having no columns. Bailing out on
        // an empty result set returned a frame with neither, so a schema
        // comparison against an empty table reported every column as removed
        // and called the change breaking.
        let path = scratch_db(
            "empty",
            "CREATE TABLE empties (id INTEGER NOT NULL, name TEXT, amount REAL);",
        );

        let frame = load(path.to_str().unwrap(), "empties").expect("load an empty table");

        assert_eq!(frame.height(), 0, "there really are no rows");
        assert_eq!(
            frame.get_column_names(),
            vec!["id", "name", "amount"],
            "but every column is still described"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn an_empty_table_compares_equal_to_a_populated_one_of_the_same_shape() {
        // The user-visible consequence, through the real comparison.
        let path = scratch_db(
            "empty_compare",
            "CREATE TABLE filled (id INTEGER NOT NULL, name TEXT);
             CREATE TABLE empties (id INTEGER NOT NULL, name TEXT);
             INSERT INTO filled (id, name) VALUES (1, 'a');",
        );
        let file = path.to_str().unwrap();

        let diff = crate::schema::run_schema_diff_frames(
            load(file, "filled").expect("load"),
            load(file, "empties").expect("load"),
            "filled",
            "empties",
        )
        .expect("compare");

        assert!(
            diff.added.is_empty(),
            "spurious additions: {:?}",
            diff.added
        );
        assert!(
            diff.removed.is_empty(),
            "an empty table is not a table missing every column: {:?}",
            diff.removed
        );

        std::fs::remove_file(&path).ok();
    }

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
        for query in [
            "SELECT * FROM t",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "SELECT 1;",
        ] {
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

    // ---- constraints and indexes ------------------------------------------

    fn catalog_of(path: &std::path::Path, table: &str) -> crate::catalog::TableCatalog {
        match read_catalog(path.to_str().unwrap(), table) {
            CatalogAvailability::Available(catalog) => catalog,
            other => panic!("expected a catalog for {table}, got {other:?}"),
        }
    }

    #[test]
    fn a_rowid_alias_primary_key_is_found() {
        // `id INTEGER PRIMARY KEY` is the commonest primary key in SQLite and
        // it creates no index at all — pragma_index_list returns nothing for
        // it. Reading the index list alone would miss it entirely, so the key
        // is taken from pragma_table_info instead.
        let path = scratch_db(
            "rowid_pk",
            "CREATE TABLE a (id INTEGER PRIMARY KEY, name TEXT);",
        );

        let catalog = catalog_of(&path, "a");
        let key = catalog.primary_key().expect("the primary key");
        assert_eq!(key.columns(), ["id"]);
        assert!(catalog.indexes.is_empty(), "{:?}", catalog.indexes);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_composite_primary_key_is_read_once_and_in_key_order() {
        // A composite key does produce an index, with origin 'pk'. Reporting
        // both that and the key from table_info would list it twice.
        let path = scratch_db(
            "composite_pk",
            "CREATE TABLE b (x INTEGER NOT NULL, y TEXT NOT NULL, PRIMARY KEY (x, y));",
        );

        let catalog = catalog_of(&path, "b");
        let keys: Vec<_> = catalog
            .constraints
            .iter()
            .filter(|c| matches!(c, crate::catalog::Constraint::PrimaryKey { .. }))
            .collect();

        assert_eq!(
            keys.len(),
            1,
            "read exactly once: {:?}",
            catalog.constraints
        );
        assert_eq!(keys[0].columns(), ["x", "y"]);
        assert!(
            catalog.indexes.is_empty(),
            "the key's own index is not listed again: {:?}",
            catalog.indexes
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn unique_constraints_and_plain_indexes_are_told_apart() {
        let path = scratch_db(
            "uniques",
            "CREATE TABLE c (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE, region TEXT);
             CREATE INDEX c_region_idx ON c (region);
             CREATE UNIQUE INDEX c_multi_idx ON c (region, email);",
        );

        let catalog = catalog_of(&path, "c");

        let uniques: Vec<_> = catalog
            .constraints
            .iter()
            .filter(|c| matches!(c, crate::catalog::Constraint::Unique { .. }))
            .collect();
        assert_eq!(uniques.len(), 1, "{:?}", catalog.constraints);
        assert_eq!(uniques[0].columns(), ["email"]);

        let mut index_names: Vec<&str> = catalog.indexes.iter().map(|i| i.name.as_str()).collect();
        index_names.sort_unstable();
        assert_eq!(index_names, ["c_multi_idx", "c_region_idx"]);

        let multi = catalog
            .indexes
            .iter()
            .find(|i| i.name == "c_multi_idx")
            .unwrap();
        assert_eq!(multi.columns, ["region", "email"], "in index order");
        assert!(multi.unique);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn check_constraints_are_declared_unreadable_rather_than_reported_as_absent() {
        // SQLite keeps CHECK bodies only in the original CREATE TABLE text.
        // Saying so is the difference between "this table has no checks" and
        // "nobody looked" — and a migration built on the wrong one would drop
        // a rule that is still there.
        let path = scratch_db(
            "checks",
            "CREATE TABLE d (id INTEGER PRIMARY KEY, amount REAL, CHECK (amount > 0));",
        );

        let catalog = catalog_of(&path, "d");
        assert!(
            catalog
                .unread
                .contains(&crate::catalog::ConstraintKind::Check),
            "the gap is declared: {:?}",
            catalog.unread
        );
        assert!(
            !catalog
                .unread
                .contains(&crate::catalog::ConstraintKind::PrimaryKey),
            "only checks are unreadable: {:?}",
            catalog.unread
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_table_missing_its_keys_reports_them() {
        let path = scratch_db(
            "compare",
            "CREATE TABLE src (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE);
             CREATE INDEX src_email_idx ON src (email);
             CREATE TABLE tgt (id INTEGER, email TEXT NOT NULL);",
        );

        let changes = crate::catalog::compare(&catalog_of(&path, "src"), &catalog_of(&path, "tgt"));

        let missing: Vec<_> = changes
            .iter()
            .filter(|c| matches!(c, crate::catalog::MetadataChange::ConstraintMissing { .. }))
            .collect();
        assert_eq!(missing.len(), 2, "primary key and unique: {changes:#?}");
        assert!(missing.iter().all(|c| c.is_breaking()));

        assert!(
            changes
                .iter()
                .any(|c| matches!(c, crate::catalog::MetadataChange::IndexMissing { .. })),
            "{changes:#?}"
        );

        std::fs::remove_file(&path).ok();
    }
}

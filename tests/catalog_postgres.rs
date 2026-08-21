//! Catalog reading against a real PostgreSQL instance.
//!
//! Ignored by default: these need a live server. Run with
//!
//! ```text
//! BIJECT_TEST_PG='postgres://postgres:test@localhost:55432/bijecttest' \
//!     cargo test --test catalog_postgres -- --ignored
//! ```
//!
//! The queries here are hand-written SQL against system catalogs, which unit
//! tests cannot check. Every connector defect found in this project so far was
//! invisible to a passing suite and obvious against a real database.

use biject::catalog::{self, CatalogAvailability, MetadataChange};
use biject::connectors::{parse_source_uri, SourceConfig};

/// Panics rather than skipping when unset.
///
/// These are `#[ignore]`, so they only run when explicitly asked for. A test
/// that silently returns and reports "ok" without connecting to anything is
/// the same silent-partial-answer failure this project keeps finding in its
/// own code; it must not be the shape of its test harness too.
fn dsn() -> String {
    std::env::var("BIJECT_TEST_PG").unwrap_or_else(|_| {
        panic!(
            "BIJECT_TEST_PG is not set, so this test would verify nothing.
             Start a database and set it, e.g.
               docker run -d --name biject-pg -e POSTGRES_PASSWORD=test \
                   -e POSTGRES_DB=bijecttest -p 55432:5432 postgres:16
               BIJECT_TEST_PG='postgres://postgres:test@localhost:55432/bijecttest'"
        )
    })
}

async fn read(dsn: &str, table: &str) -> CatalogAvailability {
    match parse_source_uri(dsn, Some(table)).expect("valid dsn") {
        SourceConfig::Postgres {
            host,
            port,
            database,
            username,
            password,
            query,
        } => {
            biject::connectors::postgres::read_catalog(
                &host,
                port.unwrap_or(5432),
                &database,
                &username,
                &password,
                &query,
            )
            .await
        }
        other => panic!("expected a postgres config, got {other:?}"),
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

#[test]
#[ignore = "needs a live PostgreSQL"]
fn reads_declared_types_nullability_and_defaults() {
    let availability = runtime().block_on(read(&dsn(), "dev"));

    let catalog = availability
        .catalog()
        .unwrap_or_else(|| panic!("expected a catalog, got {availability:?}"));

    let name = catalog.column("name").expect("name column");
    // The distinction Polars erases: both this and prod.name load as String.
    assert_eq!(name.data_type, "character varying(50)");
    assert!(name.nullable);

    let id = catalog.column("id").expect("id column");
    assert_eq!(id.data_type, "bigint");
    assert!(!id.nullable, "declared NOT NULL");

    let note = catalog.column("note").expect("note column");
    assert!(!note.nullable);
    assert!(note.default.is_some(), "has a default: {:?}", note.default);

    let amount = catalog.column("amount").expect("amount column");
    assert_eq!(amount.data_type, "numeric(12,4)", "precision is preserved");

    // Ordinals are 1-based and in declaration order.
    assert_eq!(id.ordinal, 1);
    assert_eq!(name.ordinal, 2);
}

#[test]
#[ignore = "needs a live PostgreSQL"]
fn finds_changes_a_dataframe_comparison_cannot_see() {
    let dsn = dsn();
    let rt = runtime();

    let source = rt.block_on(read(&dsn, "dev"));
    let target = rt.block_on(read(&dsn, "reporting.prod"));

    let changes = catalog::compare(
        source.catalog().expect("source catalog"),
        target.catalog().expect("target catalog"),
    );

    let rendered: Vec<String> = changes.iter().map(|c| c.to_string()).collect();

    assert!(
        rendered.iter().any(|c| c.starts_with("name: character varying(50) -> text")),
        "VARCHAR(50) to TEXT must be visible: {rendered:?}"
    );
    assert!(
        rendered.iter().any(|c| c == "note: NOT NULL dropped"),
        "{rendered:?}"
    );
    assert!(
        changes.iter().any(|c| matches!(c, MetadataChange::Default { column, .. } if column == "amount")),
        "amount gained a default: {rendered:?}"
    );
}

#[test]
#[ignore = "needs a live PostgreSQL"]
fn a_qualified_table_in_another_schema_resolves() {
    let availability = runtime().block_on(read(&dsn(), "reporting.prod"));
    assert!(availability.is_available(), "{availability:?}");
}

#[test]
#[ignore = "needs a live PostgreSQL"]
fn a_select_reports_that_there_is_no_table_to_describe() {
    let availability = runtime().block_on(read(&dsn(), "SELECT id FROM dev"));

    assert!(matches!(availability, CatalogAvailability::QueryNotATable));
    assert!(availability.explain().unwrap().contains("SELECT"));
}

#[test]
#[ignore = "needs a live PostgreSQL"]
fn a_missing_table_fails_loudly_rather_than_looking_empty() {
    // An empty catalog and a table that does not exist must not look alike.
    let availability = runtime().block_on(read(&dsn(), "no_such_table"));

    assert!(matches!(availability, CatalogAvailability::Failed(_)), "{availability:?}");
    assert!(availability.explain().unwrap().contains("no_such_table"));
}

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

/// Create the fixture tables, exactly once per test process.
///
/// Done from Rust rather than an external .sql file so a run needs only a
/// database URL — no psql on the machine, and no setup step to forget in CI.
///
/// The `Once` is load-bearing. Cargo runs a test binary's tests as threads in
/// one process, so without it every test issues the same DROP and CREATE
/// concurrently and they collide:
///
/// ```text
/// duplicate key value violates unique constraint "pg_type_typname_nsp_index"
/// ```
///
/// That only shows up against a fresh database — once the tables exist the
/// race is survivable — which made it invisible on a re-run and obvious on a
/// clean container.
fn setup(dsn: &str) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| create_fixtures(dsn));
}

fn create_fixtures(dsn: &str) {
    use tokio_postgres::NoTls;

    const DDL: &str = "
        DROP TABLE IF EXISTS dev;
        DROP TABLE IF EXISTS empties;
        DROP TABLE IF EXISTS constrained;
        DROP TABLE IF EXISTS unconstrained;
        DROP SCHEMA IF EXISTS reporting CASCADE;

        CREATE TABLE dev (
          id BIGINT NOT NULL,
          name VARCHAR(50),
          note TEXT NOT NULL DEFAULT '',
          amount NUMERIC(12,4) NOT NULL,
          created TIMESTAMPTZ DEFAULT now()
        );

        CREATE TABLE empties (
          id BIGINT NOT NULL,
          name VARCHAR(50),
          note TEXT NOT NULL DEFAULT '',
          amount NUMERIC(12,4) NOT NULL,
          created TIMESTAMPTZ DEFAULT now()
        );

        CREATE TABLE constrained (
          id BIGINT NOT NULL,
          email VARCHAR(50) NOT NULL,
          region TEXT,
          amount NUMERIC(12,2),
          CONSTRAINT constrained_pkey PRIMARY KEY (id),
          CONSTRAINT constrained_email_key UNIQUE (email),
          CONSTRAINT constrained_amount_ck CHECK (amount > 0)
        );
        CREATE INDEX constrained_region_idx ON constrained (region);
        CREATE UNIQUE INDEX constrained_multi_idx ON constrained (region, amount);
        CREATE INDEX constrained_lower_email_idx ON constrained (lower(email));

        -- The same columns with every rule removed, so a comparison isolates
        -- constraints and indexes from everything else.
        CREATE TABLE unconstrained (
          id BIGINT NOT NULL,
          email VARCHAR(50) NOT NULL,
          region TEXT,
          amount NUMERIC(12,2)
        );

        CREATE SCHEMA reporting;
        CREATE TABLE reporting.prod (
          id BIGINT NOT NULL,
          name TEXT,
          note TEXT,
          amount NUMERIC(12,4) NOT NULL DEFAULT 0,
          created TIMESTAMPTZ DEFAULT now()
        );";

    // Reuse the project's own URI parsing so the fixture speaks the same
    // dialect of connection string the tool does.
    let SourceConfig::Postgres {
        host,
        port,
        database,
        username,
        password,
        ..
    } = parse_source_uri(dsn, Some("dev")).expect("valid dsn")
    else {
        panic!("BIJECT_TEST_PG must be a postgres:// URL");
    };

    runtime().block_on(async {
        let conn_str = format!(
            "host={} port={} dbname={} user={} password={}",
            host,
            port.unwrap_or(5432),
            database,
            username,
            password
        );
        let (client, connection) = tokio_postgres::connect(&conn_str, NoTls)
            .await
            .expect("connect to the test database");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client.batch_execute(DDL).await.expect("create fixtures");
    });
}

#[test]
#[ignore = "needs a live PostgreSQL"]
fn reads_declared_types_nullability_and_defaults() {
    let dsn = dsn();
    setup(&dsn);
    let availability = runtime().block_on(read(&dsn, "dev"));

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
    setup(&dsn);
    let rt = runtime();

    let source = rt.block_on(read(&dsn, "dev"));
    let target = rt.block_on(read(&dsn, "reporting.prod"));

    let changes = catalog::compare(
        source.catalog().expect("source catalog"),
        target.catalog().expect("target catalog"),
    );

    let rendered: Vec<String> = changes.iter().map(|c| c.to_string()).collect();

    assert!(
        rendered
            .iter()
            .any(|c| c.starts_with("name: character varying(50) -> text")),
        "VARCHAR(50) to TEXT must be visible: {rendered:?}"
    );
    assert!(
        rendered.iter().any(|c| c == "note: NOT NULL dropped"),
        "{rendered:?}"
    );
    assert!(
        changes
            .iter()
            .any(|c| matches!(c, MetadataChange::Default { column, .. } if column == "amount")),
        "amount gained a default: {rendered:?}"
    );
}

#[test]
#[ignore = "needs a live PostgreSQL"]
fn a_qualified_table_in_another_schema_resolves() {
    let dsn = dsn();
    setup(&dsn);
    let availability = runtime().block_on(read(&dsn, "reporting.prod"));
    assert!(availability.is_available(), "{availability:?}");
}

#[test]
#[ignore = "needs a live PostgreSQL"]
fn a_select_reports_that_there_is_no_table_to_describe() {
    let dsn = dsn();
    setup(&dsn);
    let availability = runtime().block_on(read(&dsn, "SELECT id FROM dev"));

    assert!(matches!(availability, CatalogAvailability::QueryNotATable));
    assert!(availability.explain().unwrap().contains("SELECT"));
}

#[test]
#[ignore = "needs a live PostgreSQL"]
fn a_missing_table_is_reported_as_missing_not_as_a_failure() {
    // Three things that must not look alike: a table with no columns, a table
    // that is not there, and a lookup that went wrong. The middle one is both
    // what a typo looks like and what a table you have not created yet looks
    // like, so it gets its own answer rather than being folded into `Failed`.
    let availability = runtime().block_on(read(&dsn(), "no_such_table"));

    assert!(
        matches!(availability, CatalogAvailability::TableNotFound { .. }),
        "{availability:?}"
    );
    assert!(availability.explain().unwrap().contains("no_such_table"));
}

#[test]
#[ignore = "needs a live PostgreSQL server; set BIJECT_TEST_PG"]
fn a_table_with_no_rows_still_reports_its_columns() {
    // `dev` and `empties` are declared identically; `empties` just has no rows
    // in it. Loading a result set used to take the column list from the first
    // row, so an empty table produced a frame with no columns at all — and a
    // schema comparison against one reported every column as removed and the
    // change as breaking. An empty table is not a table without columns.
    let dsn = dsn();
    setup(&dsn);

    let config = parse_source_uri(&dsn, Some("empties")).expect("valid dsn");
    let frame = runtime()
        .block_on(biject::connectors::load_source(&config))
        .expect("load an empty table");

    assert_eq!(frame.height(), 0, "there really are no rows");
    assert_eq!(
        frame.get_column_names(),
        vec!["id", "name", "note", "amount", "created"],
        "but every column is still described"
    );
}

#[test]
#[ignore = "needs a live PostgreSQL server; set BIJECT_TEST_PG"]
fn comparing_against_an_empty_table_finds_no_differences() {
    // The user-visible consequence of the above, through the real command.
    let dsn = dsn();
    setup(&dsn);

    let load = |table: &str| {
        let config = parse_source_uri(&dsn, Some(table)).expect("valid dsn");
        runtime()
            .block_on(biject::connectors::load_source(&config))
            .expect("load")
    };

    let diff =
        biject::schema::run_schema_diff_frames(load("dev"), load("empties"), "dev", "empties")
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
    assert!(
        diff.type_changes.is_empty(),
        "spurious type changes: {:?}",
        diff.type_changes
    );
}

#[test]
#[ignore = "needs a live PostgreSQL server; set BIJECT_TEST_PG"]
fn reads_primary_keys_unique_constraints_and_checks() {
    let dsn = dsn();
    setup(&dsn);

    let CatalogAvailability::Available(catalog) = runtime().block_on(read(&dsn, "constrained"))
    else {
        panic!("expected a catalog");
    };

    let primary = catalog.primary_key().expect("a primary key");
    assert_eq!(primary.columns(), ["id"]);

    let unique: Vec<_> = catalog
        .constraints
        .iter()
        .filter(|c| matches!(c, catalog::Constraint::Unique { .. }))
        .collect();
    assert_eq!(unique.len(), 1, "{:?}", catalog.constraints);
    assert_eq!(unique[0].columns(), ["email"]);

    let checks: Vec<_> = catalog
        .constraints
        .iter()
        .filter(|c| matches!(c, catalog::Constraint::Check { .. }))
        .collect();
    assert_eq!(checks.len(), 1, "{:?}", catalog.constraints);
    assert!(
        checks[0].to_string().contains("amount"),
        "the rule text comes through: {}",
        checks[0]
    );
}

#[test]
#[ignore = "needs a live PostgreSQL server; set BIJECT_TEST_PG"]
fn reads_indexes_without_double_counting_the_ones_backing_constraints() {
    // Postgres creates an index for every primary key and unique constraint.
    // Reporting those as indexes as well would list every key in the table
    // twice, and a migration generated from it would try to create each one
    // both ways.
    let dsn = dsn();
    setup(&dsn);

    let CatalogAvailability::Available(catalog) = runtime().block_on(read(&dsn, "constrained"))
    else {
        panic!("expected a catalog");
    };

    let names: Vec<&str> = catalog
        .indexes
        .iter()
        .map(|index| index.name.as_str())
        .collect();

    assert!(
        !names
            .iter()
            .any(|name| name.contains("pkey") || *name == "constrained_email_key"),
        "constraint-backing indexes must not be listed again: {names:?}"
    );
    assert_eq!(names.len(), 3, "{names:?}");

    let multi = catalog
        .indexes
        .iter()
        .find(|index| index.name == "constrained_multi_idx")
        .expect("the multi-column index");
    assert_eq!(
        multi.columns,
        ["region", "amount"],
        "both columns, in index order"
    );
    assert!(multi.unique);

    // An expression index has no column to name. Rendering the expression
    // keeps it visible; reading pg_index.indkey directly would report a
    // column number of zero and silently lose it.
    let expression = catalog
        .indexes
        .iter()
        .find(|index| index.name == "constrained_lower_email_idx")
        .expect("the expression index");
    assert!(
        expression.columns[0].contains("lower"),
        "{:?}",
        expression.columns
    );
}

#[test]
#[ignore = "needs a live PostgreSQL server; set BIJECT_TEST_PG"]
fn a_table_missing_its_rules_reports_each_one() {
    let dsn = dsn();
    setup(&dsn);

    let (CatalogAvailability::Available(source), CatalogAvailability::Available(target)) = (
        runtime().block_on(read(&dsn, "constrained")),
        runtime().block_on(read(&dsn, "unconstrained")),
    ) else {
        panic!("expected two catalogs");
    };

    let changes = catalog::compare(&source, &target);

    let missing_constraints: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c, MetadataChange::ConstraintMissing { .. }))
        .collect();
    assert_eq!(
        missing_constraints.len(),
        3,
        "primary key, unique and check: {changes:#?}"
    );

    let missing_indexes: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c, MetadataChange::IndexMissing { .. }))
        .collect();
    assert_eq!(missing_indexes.len(), 3, "{changes:#?}");

    // Losing a uniqueness rule lets the target hold rows the source could not.
    assert!(
        missing_constraints.iter().all(|c| c.is_breaking()),
        "a rule the target does not enforce is breaking"
    );
    // A missing index is a performance problem, not a wrong answer.
    assert!(
        missing_indexes.iter().all(|c| !c.is_breaking()),
        "indexes are reported but never breaking"
    );
}

#[test]
#[ignore = "needs a live PostgreSQL server; set BIJECT_TEST_PG"]
fn a_table_compared_with_itself_reports_nothing() {
    // The check that catches identity bugs: if constraint matching keyed on
    // generated names, or normalised a check expression inconsistently, this
    // is where it shows.
    let dsn = dsn();
    setup(&dsn);

    let CatalogAvailability::Available(catalog) = runtime().block_on(read(&dsn, "constrained"))
    else {
        panic!("expected a catalog");
    };

    let changes = catalog::compare(&catalog, &catalog);
    assert!(changes.is_empty(), "{changes:#?}");
}

#[test]
#[ignore = "needs a live PostgreSQL server; set BIJECT_TEST_PG"]
fn a_failed_query_says_what_the_server_said() {
    // `tokio_postgres::Error` renders as the bare string "db error", so every
    // failure this connector reported used to be indistinguishable from every
    // other: a mistyped table, a permissions problem and a syntax error all
    // came out as "Query failed: db error". The server's message is only
    // reachable through `as_db_error()`.
    let dsn = dsn();
    setup(&dsn);

    let load = |query: &str| -> String {
        let config = parse_source_uri(&dsn, Some(query)).expect("valid dsn");
        runtime()
            .block_on(biject::connectors::load_source(&config))
            .expect_err("this query cannot succeed")
            .to_string()
    };

    let missing = load("no_such_table_at_all");
    assert!(
        missing.contains("no_such_table_at_all"),
        "names the relation: {missing}"
    );
    assert!(!missing.contains("db error"), "{missing}");

    // A different failure has to read differently, or the message carries no
    // information even when it is not empty.
    let syntax = load("SELECT FROM WHERE");
    assert!(syntax.contains("syntax error"), "{syntax}");
    assert_ne!(missing, syntax);
}

//! Constraint and index reading against live MySQL and SQL Server.
//!
//! Ignored by default: these need live servers. Run with
//!
//! ```text
//! BIJECT_TEST_MYSQL='mysql://root:test@localhost:33306/bijecttest' \
//! BIJECT_TEST_MSSQL='sqlserver://sa:Str0ng!Passw0rd@localhost:11433/bijecttest' \
//!     cargo test --test catalog_rules -- --ignored
//! ```
//!
//! PostgreSQL has its own file. These two are here because each reads rules
//! from a different set of system views — `information_schema` plus
//! `STATISTICS` on MySQL, `sys.indexes` on SQL Server — and hand-built
//! fixtures prove nothing about what those views actually return.

use biject::catalog::{self, CatalogAvailability, Constraint, ConstraintKind, TableCatalog};
use biject::connectors::parse_source_uri;

/// Panics rather than skipping when unset.
///
/// These are `#[ignore]`, so they only run when explicitly asked for. A test
/// that silently returns and reports "ok" without connecting to anything is
/// the same silent-partial-answer failure this project keeps finding in its
/// own code; it must not be the shape of its test harness too.
fn dsn(var: &str, example: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| {
        panic!("{var} is not set, so this test would verify nothing. Example: {example}")
    })
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

fn catalog_of(dsn: &str, table: &str) -> TableCatalog {
    let config = parse_source_uri(dsn, Some(table)).expect("valid dsn");
    match runtime().block_on(biject::connectors::read_catalog(&config)) {
        CatalogAvailability::Available(catalog) => catalog,
        other => panic!("no catalog for {table}: {other:?}"),
    }
}

fn kinds(catalog: &TableCatalog, wanted: ConstraintKind) -> Vec<&Constraint> {
    catalog
        .constraints
        .iter()
        .filter(|constraint| constraint.kind() == wanted)
        .collect()
}

/// Assertions that must hold on every engine, whatever views it reads from.
fn assert_reads_rules(catalog: &TableCatalog, engine: &str) {
    let key = catalog
        .primary_key()
        .unwrap_or_else(|| panic!("{engine}: no primary key found"));
    assert_eq!(key.columns(), ["id"], "{engine}");

    let unique = kinds(catalog, ConstraintKind::Unique);
    assert!(
        unique.iter().any(|c| c.columns() == ["email"]),
        "{engine}: no unique constraint on email: {:?}",
        catalog.constraints
    );

    let checks = kinds(catalog, ConstraintKind::Check);
    assert_eq!(checks.len(), 1, "{engine}: {:?}", catalog.constraints);
    assert!(
        checks[0].to_string().contains("amount"),
        "{engine}: the rule text comes through: {}",
        checks[0]
    );

    // Every engine here creates an index to enforce a primary key. Reporting
    // that index as well would list the key twice, and a migration built from
    // it would try to create the same thing two ways.
    let key_name = key.name();
    assert!(
        !catalog.indexes.iter().any(|index| index.name == key_name),
        "{engine}: the primary key's index is listed again: {:?}",
        catalog.indexes
    );

    assert!(
        catalog
            .indexes
            .iter()
            .any(|index| index.columns == ["region"] && !index.unique),
        "{engine}: the plain index is missing: {:?}",
        catalog.indexes
    );

    // Both servers expose every kind of rule; after 0.9b foreign keys are read,
    // so there should be nothing unread.
    assert!(
        catalog.unread.is_empty(),
        "{engine}: foreign keys should be read now, but unread contains: {:?}",
        catalog.unread
    );

    // Foreign key coverage.
    let fks = kinds(catalog, ConstraintKind::ForeignKey);
    assert_eq!(
        fks.len(),
        1,
        "{engine}: expected one foreign key, got {:?}",
        fks
    );
    let fk = fks[0];
    match fk {
        biject::catalog::Constraint::ForeignKey {
            columns,
            referenced_table,
            referenced_columns,
            on_delete,
            on_update,
            ..
        } => {
            assert_eq!(
                columns,
                &vec![String::from("ref_id")],
                "{engine}: foreign key columns"
            );
            // Referenced table name is schema-qualified on SQL Server, bare on MySQL.
            assert!(
                referenced_table.ends_with("fk_ref"),
                "{engine}: referenced table should end with fk_ref, got {referenced_table}"
            );
            assert_eq!(
                referenced_columns,
                &vec![String::from("id")],
                "{engine}: referenced columns"
            );
            // Actions are read as declared.
            assert_eq!(on_delete.to_string(), "CASCADE", "{engine}: on delete");
            assert_eq!(on_update.to_string(), "NO ACTION", "{engine}: on update");
        }
        _ => panic!("expected foreign key"),
    }
}

fn assert_missing_rules_are_reported(source: &TableCatalog, target: &TableCatalog, engine: &str) {
    let changes = catalog::compare(source, target);

    let missing: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c, catalog::MetadataChange::ConstraintMissing { .. }))
        .collect();
    assert!(
        missing.len() >= 3,
        "{engine}: primary key, unique and check at least: {changes:#?}"
    );
    assert!(
        missing.iter().all(|c| c.is_breaking()),
        "{engine}: a rule the target does not enforce is breaking"
    );

    assert!(
        changes
            .iter()
            .any(|c| matches!(c, catalog::MetadataChange::IndexMissing { .. })),
        "{engine}: the missing index is reported: {changes:#?}"
    );

    // Comparing a table with itself must be silent, or the identity rules are
    // keying on something generated.
    assert!(
        catalog::compare(source, source).is_empty(),
        "{engine}: a table differs from itself"
    );
}

mod mysql {
    use super::*;

    const EXAMPLE: &str = "mysql://root:test@localhost:33306/bijecttest";

    /// Cargo runs a test binary's tests as threads in one process, so without
    /// this every test in the module issues the same DROP and CREATE at once
    /// and they collide on "table already exists". The PostgreSQL suite hit
    /// exactly this and carries the same guard.
    fn setup(dsn: &str) {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| create_fixtures(dsn));
    }

    fn create_fixtures(dsn: &str) {
        use mysql_async::prelude::Queryable;

        let statements = [
            // Children before parents. A foreign key makes DROP TABLE
            // ordered, so fk_ref has to go last.
            "DROP TABLE IF EXISTS rules_source",
            "DROP TABLE IF EXISTS rules_target",
            "DROP TABLE IF EXISTS fk_ref",
            "CREATE TABLE fk_ref (id BIGINT PRIMARY KEY)",
            "CREATE TABLE rules_source (
               id BIGINT NOT NULL,
               email VARCHAR(50) NOT NULL,
               region VARCHAR(20),
               amount DECIMAL(12,2),
               ref_id BIGINT,
               PRIMARY KEY (id),
               UNIQUE KEY rules_email_key (email),
               CONSTRAINT rules_amount_ck CHECK (amount > 0),
               CONSTRAINT rules_fk FOREIGN KEY (ref_id) REFERENCES fk_ref(id) ON DELETE CASCADE ON UPDATE NO ACTION
             )",
            "CREATE INDEX rules_region_idx ON rules_source (region)",
            "CREATE TABLE rules_target (
               id BIGINT NOT NULL,
               email VARCHAR(50) NOT NULL,
               region VARCHAR(20),
               amount DECIMAL(12,2),
               ref_id BIGINT
             )",
        ];

        runtime().block_on(async {
            let pool = mysql_async::Pool::new(dsn);
            let mut conn = pool.get_conn().await.expect("connect to MySQL");
            for statement in statements {
                conn.query_drop(statement)
                    .await
                    .unwrap_or_else(|e| panic!("{statement}: {e}"));
            }
            drop(conn);
            pool.disconnect().await.ok();
        });
    }

    #[test]
    #[ignore = "needs a live MySQL server; set BIJECT_TEST_MYSQL"]
    fn reads_keys_uniques_checks_and_indexes() {
        let dsn = dsn("BIJECT_TEST_MYSQL", EXAMPLE);
        setup(&dsn);
        assert_reads_rules(&catalog_of(&dsn, "rules_source"), "mysql");
    }

    #[test]
    #[ignore = "needs a live MySQL server; set BIJECT_TEST_MYSQL"]
    fn reports_rules_the_target_lacks() {
        let dsn = dsn("BIJECT_TEST_MYSQL", EXAMPLE);
        setup(&dsn);
        assert_missing_rules_are_reported(
            &catalog_of(&dsn, "rules_source"),
            &catalog_of(&dsn, "rules_target"),
            "mysql",
        );
    }
}

mod sqlserver {
    use super::*;

    const EXAMPLE: &str = "sqlserver://sa:Str0ng!Passw0rd@localhost:11433/bijecttest";

    /// See the MySQL module: same race, same guard.
    fn setup(dsn: &str) {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| create_fixtures(dsn));
    }

    fn create_fixtures(dsn: &str) {
        use biject::connectors::SourceConfig;
        use tiberius::{AuthMethod, Client, Config};
        use tokio::net::TcpStream;
        use tokio_util::compat::TokioAsyncWriteCompatExt;

        let SourceConfig::SqlServer {
            host,
            port,
            database,
            username,
            password,
            ..
        } = parse_source_uri(dsn, Some("dbo.rules_source")).expect("valid dsn")
        else {
            panic!("BIJECT_TEST_MSSQL must be a sqlserver:// URL");
        };

        const DDL: &str = "
            -- Children before parents. A foreign key makes DROP TABLE ordered,
            -- so dropping fk_ref first fails once rules_source exists — which
            -- is to say, on every run after the first.
            IF OBJECT_ID('dbo.rules_source','U') IS NOT NULL DROP TABLE dbo.rules_source;
            IF OBJECT_ID('dbo.rules_target','U') IS NOT NULL DROP TABLE dbo.rules_target;
            IF OBJECT_ID('dbo.fk_ref','U') IS NOT NULL DROP TABLE dbo.fk_ref;
            CREATE TABLE dbo.fk_ref (id BIGINT PRIMARY KEY);
            CREATE TABLE dbo.rules_source (
              id BIGINT NOT NULL,
              email VARCHAR(50) NOT NULL,
              region VARCHAR(20),
              amount DECIMAL(12,2),
              ref_id BIGINT,
              CONSTRAINT rules_pkey PRIMARY KEY (id),
              CONSTRAINT rules_email_key UNIQUE (email),
              CONSTRAINT rules_amount_ck CHECK (amount > 0),
              CONSTRAINT rules_fk FOREIGN KEY (ref_id) REFERENCES dbo.fk_ref(id) ON DELETE CASCADE ON UPDATE NO ACTION
            );
            CREATE INDEX rules_region_idx ON dbo.rules_source (region);
            CREATE TABLE dbo.rules_target (
              id BIGINT NOT NULL,
              email VARCHAR(50) NOT NULL,
              region VARCHAR(20),
              amount DECIMAL(12,2),
              ref_id BIGINT
            );";

        // Connecting to a named database, then to the fixture database. The
        // SQL Server image has no equivalent of POSTGRES_DB or MYSQL_DATABASE,
        // so unlike the other two engines the database itself has to be
        // created here. Doing it in the test rather than in a CI step keeps
        // the suite runnable with nothing but a server and a URL — no sqlcmd
        // on the machine, no setup step to forget.
        runtime().block_on(async {
            async fn connect(
                host: &str,
                port: u16,
                database: &str,
                username: &str,
                password: &str,
            ) -> Client<tokio_util::compat::Compat<TcpStream>> {
                let mut config = Config::new();
                config.host(host);
                config.port(port);
                config.database(database);
                config.authentication(AuthMethod::sql_server(username, password));
                config.trust_cert();

                let tcp = TcpStream::connect(config.get_addr())
                    .await
                    .expect("connect to SQL Server");
                tcp.set_nodelay(true).ok();
                Client::connect(config, tcp.compat_write())
                    .await
                    .expect("authenticate")
            }

            let port = port.unwrap_or(1433);

            let mut master = connect(&host, port, "master", &username, &password).await;
            master
                .simple_query(format!(
                    "IF DB_ID('{database}') IS NULL CREATE DATABASE [{database}]"
                ))
                .await
                .expect("create the fixture database");
            drop(master);

            let mut client = connect(&host, port, &database, &username, &password).await;
            client.simple_query(DDL).await.expect("create fixtures");
        });
    }

    #[test]
    #[ignore = "needs a live SQL Server; set BIJECT_TEST_MSSQL"]
    fn reads_keys_uniques_checks_and_indexes() {
        let dsn = dsn("BIJECT_TEST_MSSQL", EXAMPLE);
        setup(&dsn);
        assert_reads_rules(&catalog_of(&dsn, "dbo.rules_source"), "sqlserver");
    }

    #[test]
    #[ignore = "needs a live SQL Server; set BIJECT_TEST_MSSQL"]
    fn reports_rules_the_target_lacks() {
        let dsn = dsn("BIJECT_TEST_MSSQL", EXAMPLE);
        setup(&dsn);
        assert_missing_rules_are_reported(
            &catalog_of(&dsn, "dbo.rules_source"),
            &catalog_of(&dsn, "dbo.rules_target"),
            "sqlserver",
        );
    }
}

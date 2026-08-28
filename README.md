# Bijection

A schema-aware data diff tool with both a Rust CLI and a Tauri desktop UI.

> **Renamed in 0.3.0.** This project was previously called `datadiff`. The crate,
> binary, and command are now `biject`. Nothing was removed and no behavior
> changed — see [MIGRATING.md](MIGRATING.md), which covers the one manual step
> for saved connection profiles.

Release notes are in [CHANGELOG.md](CHANGELOG.md).

Compare schemas for breaking changes, find modified rows with configurable tolerance, and review differences through either scripted CLI workflows or an interactive desktop app.

The project currently ships in two forms:

- `biject` — the Rust command-line interface for scripted and batch workflows
- `biject-gui` — the Tauri desktop app for interactive schema and data comparisons

## Support Matrix

| Surface | Sources | Best for |
| --- | --- | --- |
| `biject` CLI | CSV, PostgreSQL, MySQL/MariaDB, SQL Server, SQLite | automation, CI checks, manifest-driven batch runs |
| `biject-gui` desktop app | CSV, SQL Server, PostgreSQL, MySQL/MariaDB, SQLite | ad hoc inspection, side-by-side comparisons, saved connection profiles |

## Features

- **Schema Comparison** — detect added/removed columns, type changes, and compatibility issues with optional policy validation
- **Data Diffing** — identify source-only, target-only, and modified rows with configurable keys and numeric tolerance
- **Batch Operations** — compare multiple file pairs in one run with a JSON or CSV manifest
- **Policy-Driven Validation** — enforce schema contracts (required columns, forbidden removals, allowed type promotions)
- **Flexible Output** — export results as JSON or CSV for downstream automation
- **Desktop App** — side-by-side GUI built with Tauri for interactive schema and data diffing
- **Database Connectors** — SQL Server, PostgreSQL, MySQL/MariaDB, and SQLite, in both the CLI and the desktop app
- **Scalable** — optimized for large datasets with early termination and column filtering

## Installation

Tagged releases are the intended stable installation target. Source builds remain the most predictable cross-platform option.

### From Source

```bash
git clone https://github.com/vixinxiviir/biject.git
cd biject
cargo install --path .
```

This builds and installs the `biject` binary to your Cargo bin directory (usually `~/.cargo/bin`).

### Desktop App From Source

```bash
cargo build --release --manifest-path tauri-app/src-tauri/Cargo.toml
```

The desktop binary is produced at `tauri-app/src-tauri/target/release/biject-gui` on Linux and macOS, or `biject-gui.exe` on Windows.

### Release Artifacts

When available, tagged releases may include prebuilt artifacts for the CLI, the desktop app, and packaging support files. If a release does not include a binary for your platform yet, use the source build instructions above.

### Linux Runtime Dependencies

The Tauri desktop build depends on the normal Linux WebKitGTK stack. On Arch Linux, the important runtime packages are:

- `webkit2gtk-4.1`
- `gtk3`
- `libsoup3`
- `openssl`
- `librsvg`

For source builds of the current connectors, you should also expect build-time dependencies such as `rust`, `cargo`, `clang`, and `cmake`.

### Verify Installation

```bash
biject --version
biject --help
```

For the desktop app, launch:

```bash
biject-gui
```

## Quick Start

### Desktop App

Use the desktop app when you want to diff database queries or inspect changes interactively:

1. Launch `biject-gui`.
2. Choose the Data Diff or Schema Diff tab.
3. Select CSV, SQL Server, PostgreSQL, MySQL/MariaDB, or SQLite for each side.
4. For database sources, optionally save connection profiles and reuse them later.
5. Run the comparison and inspect row-level and schema-level results side by side.

### 1. Basic Schema Comparison

Compare two sources to see what columns changed. These are CSVs; a
`postgres://` URI with `--source-query` works the same way:

```bash
biject schema \
  --source gold_customers.csv \
  --target silver_customers.csv
```

Output includes:
- Columns added in target
- Columns removed from source
- Type changes and impact classification (SafePromotion, RiskyConversion, Breaking)
- Schema metadata changes — declared type, nullability, defaults, primary
  keys, unique and check constraints, and indexes — when both sides are
  database tables, or a note saying why they could not be read
- Backward and forward compatibility assessment

Options:
- `--source-query` / `--target-query` — table name or SQL query, required for database URIs
- `--policy` — path to a JSON schema policy file to assert against
- `--fail-on` — exit non-zero when changes are found at or above the given severity: `breaking` or `any`
- `--output` — file to write the comparison to; requires `--format`
- `--format` — `json` for the full result, or `csv` for a flat list of findings

CI example:
```bash
biject schema --source prod.db --source-query orders --target staging.db --target-query orders --fail-on breaking
```

Unlike `data`, `--output` here is the file itself rather than a base name, since
a schema comparison is a single document. Both formats state when schema
metadata could not be read and why, so an export showing no metadata changes is
never confused with one where the catalog was never examined.

Constraints are matched on what they do rather than what they are called, since
engines generate names: the same primary key is `customers_pkey` on PostgreSQL
and `PK__customer__3213E83F` on SQL Server. Losing a rule the source enforces is
breaking — the target can then hold data the source could not. A missing index
is reported but is never breaking, because it is slow rather than wrong.

**Scope of Comparison**

`biject schema` compares column names, declared types, nullability, defaults,
ordinal position, and table rules: primary keys, unique constraints, check
constraints, indexes and foreign keys. It does not compare:

- Triggers
- Views and materialised views
- Generated and computed column expressions
- Identity, sequence and auto-increment settings
- Collations and character sets
- Table and column comments
- Partitioning
- Storage parameters, tablespaces and fill factors
- Grants and row-level security policies
- Anything in a table other than the one named

Most of those are in the catalog and could be read; they are not read yet. The
list is printed at the end of every comparison, because "no differences found"
and "no differences among the things I looked at" are different statements and
nothing else in the output distinguishes them.

It is separate from the report's other honesty: a run also says when a catalog
could not be read at all, and when a particular kind of rule could not be read
from a particular engine. Those are about that run. This list is about the tool,
and is the same on every run.

### 2. Data Diffing with Primary Keys

Find which rows were added, removed, or modified:

```bash
biject data \
  --source gold_customers.csv \
  --target silver_customers.csv \
  --key customer_id
```

Options:
- `--key` — one or more column names for row matching (can repeat: `--key id --key date`)
- `--exclude-columns` — skip comparing certain columns (comma-separated: `--exclude-columns created_at,updated_at`)
- `--only-columns` — compare only specific columns
- `--numeric-tolerance` — maximum **absolute** difference before two numbers count as changed (`0.01` ignores differences under one hundredth, not 1%)
- `--numeric-tolerance-percent` — maximum difference as a **percentage** of the larger value (`5` ignores changes under 5%). Mutually exclusive with `--numeric-tolerance`
- `--diffs-only` — show only modified rows, skip summary tables (much faster)
- `--output` — directory to write exports; must be used together with `--format`
- `--format` — export format: `json` or `csv`; must be used together with `--output`
- `--temp` — write to a timestamped temp directory instead of `--output`; cannot be combined with `--output` or `--format`
- `--json` — emit the diff payload as JSON to stdout and suppress normal terminal output

Example with filters:

```bash
biject data \
  --source raw_events.csv \
  --target processed_events.csv \
  --key event_id \
  --exclude-columns processing_timestamp \
  --numeric-tolerance 0.001 \
  --output ./reports \
  --format json \
  --diffs-only
```

### 3. Batch Comparisons with Manifest

Run multiple file pair comparisons and get an aggregate summary:

```bash
biject batch \
  --manifest pairs.json \
  --key id \
  --output ./batch_results \
  --format json
```

Batch-specific flags:
- `--manifest-format` — force the manifest parser to `json` or `csv` instead of inferring from file extension
- `--fail-fast` — stop the batch on the first failed pair
- `--diffs-only` — show compact per-pair counts rather than fuller summaries

#### Manifest Format (JSON)

```json
[
  {
    "name": "customers_v1_to_v2",
    "source": "data/customers_v1.csv",
    "target": "data/customers_v2.csv",
    "key": "customer_id"
  },
  {
    "name": "orders_daily_check",
    "source": "data/orders_daily.csv",
    "target": "data/orders_staging.csv",
    "key": "order_id,order_date",
    "exclude_columns": "processing_notes",
    "numeric_tolerance": 0.01,
    "diffs_only": true
  }
]
```

Entries can override global settings:
- `key` (string) — override `--key` for this pair
- `exclude_columns` (string) — comma-separated columns to skip
- `only_columns` (string) — comma-separated columns to include only
- `numeric_tolerance` (float) — absolute tolerance for this pair
- `numeric_tolerance_percent` (float) — percentage tolerance for this pair; cannot be combined with `numeric_tolerance`
- `diffs_only` (bool) — show only diffs for this pair
- `output_base` (string) — per-pair output directory

#### Manifest Format (CSV)

```csv
name,source,target,key,exclude_columns,numeric_tolerance,diffs_only
customers_v1_to_v2,data/customers_v1.csv,data/customers_v2.csv,customer_id,,
orders_daily_check,data/orders_daily.csv,data/orders_staging.csv,"order_id,order_date",processing_notes,0.01,true
```

## Schema Policy & Validation

Enforce structural contracts with a JSON policy file:

```bash
biject schema \
  --source gold_schema.csv \
  --target silver_schema.csv \
  --policy schema-contract.json
```

### Policy File Format

```json
{
  "required_columns_source": ["id", "created_at"],
  "required_columns_target": ["id", "created_at", "modified_at"],
  "forbidden_removals": ["id", "customer_id"],
  "max_new_columns": 5,
  "allowed_type_changes": [
    { "from": "Int32", "to": "Int64" },
    { "from": "Float32", "to": "Float64" },
    { "from": "Int32", "to": "Int32" }
  ],
  "fail_on_breaking": true,
  "require_constraints": ["primary_key", "unique"],
  "require_primary_key_on": ["tenant_id", "id"],
  "require_indexes": ["orders_customer_idx"],
  "forbid_extra_constraints": true
}
```

- `required_columns_source` — columns that must exist in source
- `required_columns_target` — columns that must exist in target
- `forbidden_removals` — columns that cannot be removed
- `max_new_columns` — reject if more than N columns are added
- `allowed_type_changes` — list of type conversions to permit
- `fail_on_breaking` — if true, exit with error on breaking/risky changes
- `require_constraints` — constraint kinds that must not be lost (primary_key, unique, check, index)
- `require_primary_key_on` — columns that must be covered by a primary key, in order
- `require_indexes` — index or constraint names that must exist on target
- `forbid_extra_constraints` — fail if target enforces a rule source does not have

Example with primary key requirement:

```json
{
  "require_primary_key_on": ["tenant_id", "id"]
}
```

## Output & Exports

### Schema Comparison Output (terminal + optional export)

```
Schema Comparison Results
---------------------------
Source file: gold_schema.csv
Target file: silver_schema.csv

Columns added in target (1): ["new_field"]
Columns removed from source (0): []

Type changes in shared columns (1):
  - customer_id: Int32 -> Int64 (SafePromotion)

Potential renames: none

Compatibility:
  - Backward compatible: true
  - Forward compatible: false
  - Breaking reasons:
    - Added column: new_field

Policy check: passed (schema-contract.json)
```

### Data Diff Output (terminal + JSON/CSV export)

Terminal shows:
- Summary of row counts (total, source-only, target-only, modified)
- Column-level statistics (nulls, unique values, numeric min/max/mean)
- Most-changed columns

Export JSON includes structured diff results for automation.

### Batch Summary Output

```
Batch Results: 3 pairs
- customers_v1_to_v2: ✓ (5 modified rows)
- orders_daily_check: ✓ (120 target-only rows)
- transactions_staging: ✗ (missing source file)

Total: 2 succeeded, 1 failed
Total rows modified across all pairs: 125
```

## Examples

### Example 1: Validate a Data Warehouse Schema Change

```bash
# Check if a new table version is backward compatible
biject schema \
  --source warehouse/events_v2.csv \
  --target warehouse/events_v3.csv \
  --policy warehouse/schema-policies.json
```

### Example 2: Find Unexpected Changes in ETL Output

```bash
# Compare daily ETL inputs to see what changed
biject data \
  --source raw/daily_2026-03-28.csv \
  --target raw/daily_2026-03-29.csv \
  --key transaction_id \
  --diffs-only \
  --output ./etl_check \
  --format json
```

### Example 3: Batch Validation After Release

```bash
# Run schema checks on all updated tables after a deployment
biject schema \
  --source prod_snapshot.csv \
  --target staging_snapshot.csv \
  --policy prod-schema-contract.json

# If schema is OK, check data integrity
biject batch \
  --manifest prod_validation_pairs.json \
  --key id \
  --output ./release_validation \
  --format json
```

## Performance Tips

- Use `--diffs-only` to skip expensive statistics computation
- Use `--exclude-columns` or `--only-columns` to reduce comparison scope
- For multi-column keys, use only the minimal key set needed for matching
- Test policy files on small samples before batch runs

## Troubleshooting

**Error: "No columns added in target"**  
Normal when schemas match. Check file paths and CSV encoding.

**Error: "CSV parsing failed"**  
Verify the input is a standard CSV with the expected delimiter, quotes, and encoding.

**`--output` or `--format` is rejected**  
Use them together. The CLI requires `--output` and `--format` as a pair, while `--temp` is an alternative output mode.

**Error: "... has N duplicate values for key column ..."**  
The key you chose is not unique in that file, so rows cannot be paired one-to-one. Add another `--key` column until the combination is unique, or de-duplicate the input. Bijection refuses to guess here rather than silently comparing only one of the matching rows.

**Batch run fails on one pair but not others**  
Run the failing pair directly with `biject data` using the same filters, or rerun the batch with `--fail-fast` to stop at the first failing entry.

**Connecting to a database from the CLI**  
Pass a URI as `--source` or `--target`, with `--source-query` / `--target-query` giving a table name or SQL query:

```bash
biject schema   --source postgres://user:pass@localhost:5432/dev --source-query public.orders   --target postgres://user:pass@localhost:5432/prod --target-query public.orders
```

Supported schemes are `postgres://`, `mysql://`, `sqlserver://` and `sqlite://`. Anything else is treated as a CSV path.

**Schema metadata is not being compared**  
Declared types, nullability, defaults, constraints and indexes come from the database catalog, which needs a table rather than a query — an arbitrary `SELECT` has no single table to describe, and a CSV has no catalog at all. The report always says which side is missing and why. All four engines read metadata.

**A kind of constraint is listed as not compared**  
Not every engine exposes every kind. SQLite keeps `CHECK` bodies only in the original `CREATE TABLE` text, and MySQL before 8.0.16 has no `CHECK_CONSTRAINTS` view. Rather than treat an empty list as "this table has none", the report names the kind and the side that could not read it, and skips comparing that kind entirely.

**Check constraints differ between two engines that look the same**  
Each server re-renders a `CHECK` body into its own spelling — `(amount > (0)::numeric)` on PostgreSQL, ``(`amount` > 0)`` on MySQL, `([amount]>(0))` on SQL Server. They compare reliably within one engine, not across two. `CREATE UNIQUE INDEX` is likewise a unique *constraint* on MySQL and an *index* on PostgreSQL and SQL Server; each reading is faithful to its own server.

**Foreign keys are not reported**  
PostgreSQL reads them. MySQL, SQL Server and SQLite do not yet, and say so in the report rather than leaving an empty list that would read as "this table has none". The referenced table is recorded as a name only — nothing connects to it or checks that it exists, because every comparison here works on one table at a time.

**Type classification seems wrong**  
Polars infers schema from the first 100 rows. If a CSV column contains mixed types, normalize the input first so the sampled rows reflect the full dataset.

## Contributing

Contributions are welcome. Open an issue for bugs, feature requests, or release packaging problems, and use pull requests for scoped changes.

Please read [CONTRIBUTING.md](CONTRIBUTING.md) first — it covers the development
workflow and the inbound license grant that pull requests require. Commits need
a `Signed-off-by:` line (`git commit -s`).

## License

Bijection is free software under **GPL-3.0-only**. See [LICENSE](LICENSE).

Commercial licenses are available for use that cannot comply with the GPL, and
`biject migrate` — migration and rollback DDL generation — is a separate paid
product. See [LICENSING.md](LICENSING.md) for the full picture, including the
commitment that nothing currently free will become paid.

Dependency licenses are inventoried in [docs/licensing.md](docs/licensing.md).
The project name is covered by the [NOTICE](NOTICE) file, not by the GPL.

## Roadmap

Near term:

- [ ] Foreign keys in schema comparison for every engine — PostgreSQL reads them today
- [ ] A report that names what it never examines, so a clean result says how much it covered
- [ ] Release binaries for macOS and Windows alongside Linux
- [ ] Desktop app level with the CLI, or a plain statement of what it does not cover
- [ ] A documented, stable public API for 1.0

Later:

- [ ] Checksum and sampling comparison for warehouse-scale tables — the gate on everything below
- [ ] Cloud warehouse connectors (Snowflake, Databricks, BigQuery)
- [ ] Cross-engine schema diff and type mapping

Explicitly not planned: hosted or scheduled components, subscriptions, accounts,
and any mode that executes generated DDL against a live database.

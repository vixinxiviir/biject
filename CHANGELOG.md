# Changelog

Notable changes to Bijection. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Bijection was called `datadiff` before 0.3.0. See [MIGRATING.md](MIGRATING.md).

---

## [Unreleased]

### Added

- **Primary keys, unique constraints, check constraints and indexes** in
  schema comparison, on all four engines. A comparison used to see columns and
  nothing below them, so a table could be reported as matching while permitting
  duplicates the other forbade and scanning where the other used an index.
  Losing a rule the source enforces is breaking; a missing index is reported
  but never breaking, because it is slow rather than wrong.
- **Constraints are matched on what they do, not what they are called.**
  Engines invent names — `customers_pkey`, `PK__customer__3213E83F` — so
  matching on them would report a difference on nearly every comparison.
- **Kinds a connector cannot read are declared, not assumed absent.** SQLite
  keeps `CHECK` bodies only in the original `CREATE TABLE` text, and MySQL
  before 8.0.16 has no `CHECK_CONSTRAINTS` view at all. Both now say so, and
  comparison skips that kind rather than reporting every rule on the other
  side as missing.

### Changed

- **Breaking:** `TableCatalog` is `#[non_exhaustive]` and carries
  `constraints`, `indexes` and `unread`. Build one with `TableCatalog::new`
  and the `with_` methods rather than a struct literal.
- **Breaking:** `MetadataChange::column` is now `subject`. A constraint spans
  zero or many columns, so there is not always one to return.

### Known limits

- **Foreign keys are not modelled.** They reference another table, and
  everything here works on one table at a time.
- **Check expressions compare within an engine, not across two.** Each server
  re-renders the body into its own spelling: `(amount > (0)::numeric)` on
  PostgreSQL, ``(`amount` > 0)`` on MySQL, `([amount]>(0))` on SQL Server.
- **`CREATE UNIQUE INDEX` classifies differently by engine** — a unique
  constraint on MySQL, an index on PostgreSQL and SQL Server. Each reading is
  faithful to its own server, so a cross-engine comparison reports it.

### Fixed

- **A table with no rows was read as a table with no columns.** Every
  connector took the column list from the first row returned, so an empty
  result set produced a frame with nothing in it. Comparing anything against
  an empty table therefore reported every column as removed and the change as
  breaking — a confident, completely wrong answer, and the exact failure this
  project keeps hunting. PostgreSQL now takes the description from the
  prepared statement, SQL Server from its result-set metadata, and MySQL and
  SQLite keep the column list they had already read. Verified against a live
  server on all four.

---

## [0.5.0] — 2026-08-24

Schema comparison now reads the database's own catalog, so it reports what a
DataFrame cannot show: declared types, nullability, and defaults.

Before this release, comparing two PostgreSQL tables where a column changed from
`VARCHAR(50)` to `TEXT` reported no difference at all. Both load as a string, so
the comparison had nothing to distinguish them by.

### Added

- **Column metadata in schema comparison.** Declared type, nullability, default
  expression, and column position, read from `information_schema` on
  PostgreSQL, MySQL and SQL Server, and from `pragma_table_info` on SQLite.
- **`schema --output` and `--format`.** Write a comparison to a file as `json`
  (the full result) or `csv` (a flat list of findings). `--output` is the file
  itself, not a base name inside a generated folder.
- **Column metadata in the desktop app**, matching the CLI.
- **`biject --version`.** The README documented it; it did not exist.

### Changed

- **Compatibility now accounts for metadata.** A dropped `NOT NULL` or a
  narrowed column makes a comparison backward-incompatible. Previously the
  verdict was computed only from column additions, removals, and inferred
  types, so it could report "backward compatible" beneath changes it had just
  listed as breaking.
- **Type changes are classified as widening or narrowing.** `varchar(50)` to
  `varchar(200)` is a widening and is not breaking; the reverse can truncate
  and is. Changes that cross type families, such as `varchar(50)` to `text`,
  are not classified and remain conservatively breaking.
- **Metadata gaps are stated, never implied.** When a catalog cannot be read —
  a file source, a `SELECT` rather than a table, a failed lookup — the report
  says which side and why, in every output format. An empty list of metadata
  changes never has to be interpreted.

### Fixed

- The README stated, under a troubleshooting heading, that database sources
  were "not available in the CLI". They have been since 0.3.0. The support
  matrix and feature list said the same, so a reader would conclude a headline
  capability did not exist.

---

## [0.4.0] — 2026-08-19

Database connectors preserved no type information. Every value from every
database arrived as a string, which broke more than it appeared to.

### Fixed

- **`--numeric-tolerance` did nothing on database sources.** Because every
  column arrived as text, the numeric comparison never applied. Two tables
  differing by 0.005 still reported as changed under `--numeric-tolerance 0.01`.
- **`schema` never reported a type change on a database source**, since both
  sides compared as text.
- **PostgreSQL `NUMERIC` columns decoded as null.** Any diff over money or
  decimals was comparing nothing on both sides and reporting a match.
- **SQL Server panicked** on any table containing a non-string column, which is
  essentially every real table. Its advertised support had never worked.
- **Duplicate composite keys silently dropped rows.** Rows sharing a key
  overwrote one another and only the last was compared, with no warning. This
  is now an error naming the offending values.
- **`--numeric-tolerance` documented percentage semantics it never had.**
  `0.05` was always an absolute difference, never five percent. The
  documentation was wrong, not the behaviour.

### Added

- **`--numeric-tolerance-percent`**, a proportional tolerance. Mutually
  exclusive with `--numeric-tolerance`.
- **`THIRD-PARTY-NOTICES.txt`**, shipped with every binary and installed by the
  AUR package. Generation fails the release if a dependency carries an
  unreviewed licence.
- Contributor licence agreement, dependency licence audit, and migration notes.
- A test suite, from 3 tests to 113, run in CI on every push.

### Changed

- **Breaking:** `SchemaDiffResult` is `#[non_exhaustive]` and carries
  `source_schema` and `target_schema`. `TypeChange`, `RenameSuggestion` and
  `CompatibilitySummary` expose their fields, and `TypeChangeImpact` is public.
  Previously these were public types whose contents could not be read.
- Integer widths, unsigned flags, dates and timestamps are preserved per
  connector rather than flattened to text.

---

## [0.3.0] — 2026-08-15

Renamed from `datadiff` to **Bijection**. The crate, binary, and command are
all `biject`.

### Added

- Database connections from the CLI, not only the desktop app: `postgres://`,
  `mysql://`, `sqlserver://` and `sqlite://` URIs with `--source-query` and
  `--target-query`.
- `biject::cli`, exposing the free command set so a downstream binary can embed
  it without duplicating argument definitions.

### Changed

- **Breaking:** every name. Binary `datadiff` to `biject`, desktop app
  `datadiff-gui` to `biject-gui`, crate `datadiff` to `biject`.
- **Breaking:** connection profiles and saved passwords do not carry over. The
  profile directory and the OS keychain service are both named after the
  application. See [MIGRATING.md](MIGRATING.md).
- The AUR package `datadiff` was merged into `biject`.

---

## [0.2.2] — 2026-04-02

- Packaging fixes: system SQLite in the PKGBUILD, icons restored so the release
  workflow could complete.

## [0.2.1] — 2026-04-02

- Release packaging corrections.

## [0.2.0] — 2026-04-02

First tagged release, as `datadiff`. Schema comparison, keyed row diffs, batch
manifests, schema policies, and a Tauri desktop app with connectors for SQL
Server, PostgreSQL, MySQL/MariaDB and SQLite.

[Unreleased]: https://github.com/vixinxiviir/biject/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/vixinxiviir/biject/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/vixinxiviir/biject/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/vixinxiviir/biject/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/vixinxiviir/biject/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/vixinxiviir/biject/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/vixinxiviir/biject/releases/tag/v0.2.0

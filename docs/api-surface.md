# Public API surface

A snapshot of the `bijection` crate's public surface, taken on 2026-08-28
against 0.9, to decide what should stay public at 1.0. **It goes stale the
moment anything changes** — regenerate rather than trust the date.

## What is listed

Types, free functions, methods and constants. Struct fields and enum variants
are deliberately left out: they cannot be sealed independently of the type that
holds them, so listing them adds two hundred rows and no decisions.

## What the last two columns mean

**Used outside its own module** — the identifier appears somewhere else in this
repository. Computed by `git grep -w`, so it matches on name alone: an item
sharing a name with something unrelated reads as used. It errs towards "used",
which is the safe direction here, since the cost of wrongly sealing something is
higher than the cost of leaving something public one release longer.

**Used outside the crate** — `yes` where a use is visible in `tests/` or
`tauri-app/`. Otherwise **`unknown`, never `no`**: the paid crate is a separate
private repository and downstream users are not visible from here. Reading an
`unknown` as "nobody uses this" is exactly the mistake this column is worded to
prevent.

## What stands out

**`sqldialect` has no consumer inside this crate at all.** `Dialect`,
`UnsupportedType`, `quote_ident`, `quote_table` and `render_type` are used
nowhere in `src/` outside their own file. That is deliberate, not dead code: how
an engine spells a type and quotes an identifier is knowledge about databases, so
it lives in the GPL half where it is reviewable and covered by the open test
suite, while the paid crate is the thing that turns it into statements. Sealing
it would break the paid build and take the reviewable half of multi-dialect
support private. See `docs/specs/README.md`, "What the local implementer does and
does not get".

**Most of `schema`'s result types are unused in-crate for the same reason.**
`TypeChange`, `RenameSuggestion`, `CompatibilitySummary`, `Scope` and the error
enums are what an embedder reads off a `SchemaDiffResult`. They are produced in
one module and consumed outside the crate.

That is the pattern to hold in mind when reading the "no" rows below: this crate
is a library with a private downstream, so **"unused here" is not evidence of
"unused"**.

| Item | Kind | Used outside its own module? | Used outside the crate? |
| --- | --- | --- | --- |
| `catalog::ColumnDef` | struct | yes | unknown |
| `catalog::Constraint` | enum | yes | yes |
| `catalog::ConstraintKind` | enum | yes | yes |
| `catalog::ReferentialAction` | enum | yes | unknown |
| `catalog::IndexDef` | struct | yes | unknown |
| `catalog::TableCatalog` | struct | yes | yes |
| `catalog::CatalogAvailability` | enum | yes | yes |
| `catalog::TypeImpact` | enum | yes | unknown |
| `catalog::MetadataChange` | enum | yes | yes |
| `catalog::ColumnDef::new` | method | yes | yes |
| `catalog::ConstraintKind::ALL` | method | yes | unknown |
| `catalog::Constraint::kind` | method | yes | yes |
| `catalog::Constraint::name` | method | yes | yes |
| `catalog::Constraint::columns` | method | yes | yes |
| `catalog::IndexDef::new` | method | yes | yes |
| `catalog::TableCatalog::new` | method | yes | yes |
| `catalog::TableCatalog::with_constraints` | method | yes | unknown |
| `catalog::TableCatalog::with_indexes` | method | yes | unknown |
| `catalog::TableCatalog::with_unread` | method | yes | unknown |
| `catalog::TableCatalog::by_name` | method | yes | unknown |
| `catalog::TableCatalog::column` | method | yes | yes |
| `catalog::TableCatalog::primary_key` | method | yes | yes |
| `catalog::CatalogAvailability::catalog` | method | yes | yes |
| `catalog::CatalogAvailability::is_available` | method | yes | yes |
| `catalog::CatalogAvailability::explain` | method | yes | yes |
| `catalog::classify_type_change` | function | yes | unknown |
| `catalog::MetadataChange::subject` | method | yes | unknown |
| `catalog::MetadataChange::is_breaking` | method | yes | yes |
| `catalog::compare` | function | yes | yes |
| `cli::Cli` | struct | yes | unknown |
| `cli::Commands` | enum | yes | yes |
| `cli::dispatch` | function | yes | unknown |
| `connectors::SourceConfig` | enum | yes | yes |
| `connectors::ConnectorError` | enum | yes | unknown |
| `connectors::SourceConfig::label` | method | yes | yes |
| `connectors::parse_source_uri` | function | yes | yes |
| `connectors::profiles::ConnectionProfile` | struct | yes | yes |
| `connectors::profiles::ProfileError` | enum | yes | yes |
| `connectors::profiles::list_profiles` | function | yes | yes |
| `connectors::profiles::save_profile` | function | yes | yes |
| `connectors::profiles::update_profile` | function | yes | yes |
| `connectors::profiles::delete_profile` | function | yes | yes |
| `connectors::profiles::get_password` | function | yes | yes |
| `data::ExportFormat` | enum | yes | unknown |
| `data::FailOn` | enum | yes | unknown |
| `data::ManifestFormat` | enum | yes | unknown |
| `data::DataDiffError` | enum | no | unknown |
| `data::Tolerance` | enum | yes | yes |
| `data::Tolerance::resolve` | method | yes | yes |
| `data::data_diff` | function | yes | unknown |
| `data::batch_diff` | function | yes | unknown |
| `data::validate_export_args` | function | yes | unknown |
| `data::run_diff` | function | yes | yes |
| `data::run_diff_frames` | function | yes | yes |
| `schema::TypeChangeImpact` | enum | no | unknown |
| `schema::SchemaDiffError` | enum | no | unknown |
| `schema::TypeChange` | struct | no | unknown |
| `schema::RenameSuggestion` | struct | no | unknown |
| `schema::CompatibilitySummary` | struct | no | unknown |
| `schema::SchemaDiffResult` | struct | yes | yes |
| `schema::MetadataReport` | struct | yes | unknown |
| `schema::Scope` | struct | no | unknown |
| `schema::MetadataReport::is_complete` | method | no | unknown |
| `schema::MetadataReport::gaps` | method | yes | yes |
| `schema::MetadataReport::unread_constraints` | method | yes | unknown |
| `schema::Scope::of_this_tool` | method | no | unknown |
| `schema::run_schema_diff_frames_with_catalog` | function | yes | yes |
| `schema::run_schema_diff_frames` | function | yes | yes |
| `schema::run_schema_diff` | function | yes | yes |
| `schema::schema_diff` | function | yes | unknown |
| `sqldialect::Dialect` | enum | no | unknown |
| `sqldialect::UnsupportedType` | struct | no | unknown |
| `sqldialect::Dialect::name` | method | yes | yes |
| `sqldialect::Dialect::quote_ident` | method | no | unknown |
| `sqldialect::Dialect::quote_table` | method | no | unknown |
| `sqldialect::Dialect::of` | method | yes | yes |
| `sqldialect::Dialect::render_type` | method | no | unknown |
| `sqltype::CanonicalType` | struct | yes | unknown |
| `sqltype::UNBOUNDED` | const | yes | unknown |
| `sqltype::canonical` | function | yes | unknown |

//! Schema as the database itself describes it.
//!
//! Comparing DataFrames tells you a column is a string. It cannot tell you the
//! column is `VARCHAR(50) NOT NULL DEFAULT ''`, because none of that survives
//! being loaded into Polars. Two genuinely different Postgres columns —
//! `VARCHAR(50)` and `TEXT` — arrive identical, so a real schema change is
//! invisible.
//!
//! This module reads the catalog instead: `information_schema` on PostgreSQL,
//! MySQL and SQL Server, `PRAGMA` on SQLite.
//!
//! **A catalog is not always available**, and that is the important case. CSV
//! files have no catalog at all, and an arbitrary `SELECT` has no single table
//! to describe. Rather than silently reporting less, callers get
//! [`CatalogAvailability`] telling them exactly what could not be read and why,
//! so the difference between "nothing changed" and "nothing was checked" stays
//! visible.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;

/// One column, as the database declares it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ColumnDef {
    pub name: String,
    /// The native type, as written in the catalog: `character varying(50)`,
    /// `bigint`, `timestamp with time zone`. Not a Polars type — the whole
    /// point is to see distinctions Polars erases.
    pub data_type: String,
    pub nullable: bool,
    /// Default expression as the catalog stores it, e.g. `now()` or `'0'::text`.
    pub default: Option<String>,
    /// 1-based position, so column reordering is visible.
    pub ordinal: u32,
}

/// A rule the table enforces about its own rows.
///
/// Deliberately table-local: every kind here mentions only this table's own
/// columns. **Foreign keys are not modelled.** They point at another table, and
/// everything that consumes a catalog here works on one table at a time, so a
/// key referencing something nobody has looked at could be reported but never
/// acted on — and generating DDL for one means ordering work across tables,
/// which is a different program from this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Constraint {
    PrimaryKey {
        name: String,
        /// In key order, which matters: `(a, b)` and `(b, a)` serve different
        /// queries even though they forbid the same duplicates.
        columns: Vec<String>,
    },
    Unique {
        name: String,
        columns: Vec<String>,
    },
    /// A row-level rule, such as `amount > 0`.
    ///
    /// The expression is the catalog's own spelling rather than what was
    /// typed — PostgreSQL stores `CHECK (amount > 0)` and hands back
    /// `((amount > 0))`. Comparable within one engine, not across two.
    Check {
        name: String,
        expression: String,
    },
}

/// A kind of table-level rule.
///
/// Exists so a catalog can say *which* kinds it managed to read. Not every
/// engine exposes every kind: SQLite publishes primary keys and unique
/// constraints through pragmas but keeps `CHECK` bodies only in the original
/// `CREATE TABLE` text, which this does not parse. Without recording that, an
/// empty list of checks would be indistinguishable from a table that has none
/// — and a migration generated from it would confidently drop a rule it had
/// simply never seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    PrimaryKey,
    Unique,
    Check,
    Index,
}

impl ConstraintKind {
    pub const ALL: [ConstraintKind; 4] = [
        ConstraintKind::PrimaryKey,
        ConstraintKind::Unique,
        ConstraintKind::Check,
        ConstraintKind::Index,
    ];
}

impl fmt::Display for ConstraintKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            ConstraintKind::PrimaryKey => "primary keys",
            ConstraintKind::Unique => "unique constraints",
            ConstraintKind::Check => "check constraints",
            ConstraintKind::Index => "indexes",
        };
        f.write_str(label)
    }
}

impl Constraint {
    pub fn kind(&self) -> ConstraintKind {
        match self {
            Constraint::PrimaryKey { .. } => ConstraintKind::PrimaryKey,
            Constraint::Unique { .. } => ConstraintKind::Unique,
            Constraint::Check { .. } => ConstraintKind::Check,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Constraint::PrimaryKey { name, .. }
            | Constraint::Unique { name, .. }
            | Constraint::Check { name, .. } => name,
        }
    }

    /// The columns a constraint covers, empty for a `CHECK`.
    pub fn columns(&self) -> &[String] {
        match self {
            Constraint::PrimaryKey { columns, .. } | Constraint::Unique { columns, .. } => columns,
            Constraint::Check { .. } => &[],
        }
    }

    /// What makes two constraints the same constraint.
    ///
    /// Names are excluded on purpose. Engines invent them — `customers_pkey`
    /// on PostgreSQL, `PK__customer__3213E83F` on SQL Server — so two
    /// identical primary keys routinely carry different names. Comparing on
    /// name would report a difference where there is none, on every single
    /// cross-engine comparison.
    fn identity(&self) -> (&'static str, String) {
        match self {
            Constraint::PrimaryKey { columns, .. } => ("primary_key", columns.join(",")),
            Constraint::Unique { columns, .. } => ("unique", columns.join(",")),
            Constraint::Check { expression, .. } => ("check", normalize_expression(expression)),
        }
    }
}

impl fmt::Display for Constraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Constraint::PrimaryKey { columns, .. } => {
                write!(f, "PRIMARY KEY ({})", columns.join(", "))
            }
            Constraint::Unique { columns, .. } => write!(f, "UNIQUE ({})", columns.join(", ")),
            Constraint::Check { expression, .. } => write!(f, "CHECK {expression}"),
        }
    }
}

/// A lookup structure.
///
/// Not a correctness rule, which is why it is kept apart from [`Constraint`].
/// A missing index does not give a wrong answer; it turns a working query into
/// a table scan. That is a production incident rather than a corruption, and
/// worth reporting for the same reason.
///
/// Indexes that exist only to back a primary key or unique constraint are left
/// out by the connectors that read them. The constraint already reports them,
/// and listing both would double-count every key in the table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndexDef {
    pub name: String,
    /// In index order, which determines the queries it can serve.
    pub columns: Vec<String>,
    pub unique: bool,
}

impl IndexDef {
    /// Two indexes are the same index when they cover the same columns in the
    /// same order with the same uniqueness. Names are generated, so they are
    /// no more comparable here than they are for constraints.
    fn identity(&self) -> (String, bool) {
        (self.columns.join(","), self.unique)
    }
}

impl fmt::Display for IndexDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = if self.unique { "UNIQUE INDEX" } else { "INDEX" };
        write!(f, "{kind} ({})", self.columns.join(", "))
    }
}

/// A table as the database describes it: its columns, the rules it enforces,
/// and the indexes over it.
///
/// `#[non_exhaustive]`: this grew from columns alone in 0.6, and it will grow
/// again. Build one with [`TableCatalog::new`] and the `with_` methods rather
/// than a struct literal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct TableCatalog {
    pub columns: Vec<ColumnDef>,
    pub constraints: Vec<Constraint>,
    pub indexes: Vec<IndexDef>,
    /// Constraint kinds this connector could not read for this table.
    ///
    /// Anything listed here is absent from `constraints` or `indexes` because
    /// it was never looked at, not because the table has none. Comparison
    /// skips these kinds outright rather than reporting every rule on the
    /// other side as missing.
    pub unread: Vec<ConstraintKind>,
}

impl TableCatalog {
    pub fn new(columns: Vec<ColumnDef>) -> Self {
        Self {
            columns,
            ..Self::default()
        }
    }

    pub fn with_constraints(mut self, constraints: Vec<Constraint>) -> Self {
        self.constraints = constraints;
        self
    }

    pub fn with_indexes(mut self, indexes: Vec<IndexDef>) -> Self {
        self.indexes = indexes;
        self
    }

    /// Declare kinds this connector cannot read, so their absence is not read
    /// as evidence that the table has none.
    pub fn with_unread(mut self, unread: Vec<ConstraintKind>) -> Self {
        self.unread = unread;
        self
    }

    fn reads(&self, kind: ConstraintKind) -> bool {
        !self.unread.contains(&kind)
    }

    pub fn by_name(&self) -> BTreeMap<&str, &ColumnDef> {
        self.columns
            .iter()
            .map(|column| (column.name.as_str(), column))
            .collect()
    }

    pub fn column(&self, name: &str) -> Option<&ColumnDef> {
        self.columns.iter().find(|column| column.name == name)
    }

    pub fn primary_key(&self) -> Option<&Constraint> {
        self.constraints
            .iter()
            .find(|constraint| matches!(constraint, Constraint::PrimaryKey { .. }))
    }
}

/// Whether catalog metadata could be read, and if not, why not.
///
/// Modelled explicitly rather than as `Option<TableCatalog>` so the reason
/// reaches the user. "No nullability changes" and "nullability was never
/// examined" are different statements and must not look alike.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CatalogAvailability {
    /// The catalog was read.
    Available(TableCatalog),
    /// The source is a file. Files have no catalog.
    NotADatabase,
    /// The source is a database, but the query is a `SELECT` rather than a
    /// table reference, so there is no single table to describe.
    QueryNotATable,
    /// The source is a database and the query names a table, but reading the
    /// catalog failed. Carries the reason; never silently treated as absent.
    ///
    /// A struct variant, not a newtype: serde cannot serialise a tagged newtype
    /// variant holding a plain string, and this enum is internally tagged.
    Failed { reason: String },
    /// This connector cannot read catalogs yet. Distinct from a failure: the
    /// lookup was never attempted because the code does not exist.
    UnsupportedEngine { engine: String },
    /// The caller did not ask for metadata. Distinct from every other variant:
    /// nothing was wrong, nothing was tried.
    NotRequested,
}

impl CatalogAvailability {
    pub fn catalog(&self) -> Option<&TableCatalog> {
        match self {
            CatalogAvailability::Available(catalog) => Some(catalog),
            _ => None,
        }
    }

    pub fn is_available(&self) -> bool {
        matches!(self, CatalogAvailability::Available(_))
    }

    /// Why metadata is missing, phrased for a user reading a diff report.
    pub fn explain(&self) -> Option<String> {
        match self {
            CatalogAvailability::Available(_) => None,
            CatalogAvailability::NotADatabase => {
                Some("file sources have no catalog to read".to_string())
            }
            CatalogAvailability::QueryNotATable => Some(
                "the query is a SELECT rather than a table reference, so there is no \
                 single table to describe"
                    .to_string(),
            ),
            CatalogAvailability::Failed { reason } => {
                Some(format!("reading the catalog failed: {reason}"))
            }
            CatalogAvailability::UnsupportedEngine { engine } => Some(format!(
                "{engine} catalog reading is not implemented yet"
            )),
            CatalogAvailability::NotRequested => {
                Some("metadata was not requested".to_string())
            }
        }
    }
}

/// How a change of declared type affects something reading the column.
///
/// Only same-family changes are classified. Deciding that `varchar(50)` to
/// `text` is safe requires knowing each engine's type families, which is a
/// per-dialect equivalence matrix and deliberately out of scope; those stay
/// `Unknown` and are treated as breaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeImpact {
    /// Same type, more capacity. `varchar(50)` to `varchar(200)`. Safe.
    Widening,
    /// Same type, less capacity. `varchar(200)` to `varchar(50)`. Can truncate.
    Narrowing,
    /// Different base types, or parameters that cannot be compared.
    Unknown,
}

/// Sentinel for an unbounded length, such as SQL Server's `varchar(max)`.
const UNBOUNDED: u64 = u64::MAX;

/// Classify a change between two declared types.
pub fn classify_type_change(from: &str, to: &str) -> TypeImpact {
    let (from_base, from_params) = split_type(from);
    let (to_base, to_params) = split_type(to);

    if from_base != to_base || from_params.len() != to_params.len() {
        return TypeImpact::Unknown;
    }

    // Equal parameter counts on the same base: compare position by position.
    let grew = from_params
        .iter()
        .zip(&to_params)
        .any(|(before, after)| after > before);
    let shrank = from_params
        .iter()
        .zip(&to_params)
        .any(|(before, after)| after < before);

    match (grew, shrank) {
        // Anything smaller can lose data, even if something else grew:
        // decimal(12,4) to decimal(18,2) keeps more digits and fewer decimals.
        (_, true) => TypeImpact::Narrowing,
        (true, false) => TypeImpact::Widening,
        (false, false) => TypeImpact::Unknown,
    }
}

/// Split `varchar(50)` into `("varchar", [50])`.
///
/// A non-numeric parameter other than `max` yields no parameters, which forces
/// `Unknown` rather than a guess.
fn split_type(declared: &str) -> (String, Vec<u64>) {
    let trimmed = declared.trim().to_ascii_lowercase();
    let Some(open) = trimmed.find('(') else {
        return (trimmed, Vec::new());
    };
    let base = trimmed[..open].trim().to_string();
    let Some(close) = trimmed.rfind(')') else {
        return (trimmed, Vec::new());
    };

    let mut params = Vec::new();
    for part in trimmed[open + 1..close].split(',') {
        let part = part.trim();
        if part == "max" {
            params.push(UNBOUNDED);
        } else if let Ok(value) = part.parse::<u64>() {
            params.push(value);
        } else {
            // An unparseable parameter means the comparison cannot be trusted.
            return (base, Vec::new());
        }
    }

    (base, params)
}

/// A change to a column that only the catalog can reveal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetadataChange {
    /// The declared type changed in a way Polars cannot see, such as
    /// `character varying(50)` becoming `text`.
    NativeType {
        column: String,
        from: String,
        to: String,
        impact: TypeImpact,
    },
    /// A column became nullable, or stopped being.
    Nullability {
        column: String,
        /// True when the target permits nulls.
        now_nullable: bool,
    },
    Default {
        column: String,
        from: Option<String>,
        to: Option<String>,
    },
    /// Position changed. Rarely meaningful, but `SELECT *` consumers care.
    Ordinal {
        column: String,
        from: u32,
        to: u32,
    },
    /// A constraint the source enforces and the target does not.
    ConstraintMissing { constraint: Constraint },
    /// A constraint the target enforces and the source does not.
    ConstraintExtra { constraint: Constraint },
    /// An index the source has and the target lacks.
    IndexMissing { index: IndexDef },
    /// An index the target has and the source lacks.
    IndexExtra { index: IndexDef },
}

impl MetadataChange {
    /// What the change is about: a column name, or a constraint or index name.
    ///
    /// Renamed from `column` in 0.6, when constraints and indexes arrived and
    /// the old name stopped being true. A constraint spans zero or many
    /// columns, so there is not always one to return.
    pub fn subject(&self) -> &str {
        match self {
            MetadataChange::NativeType { column, .. }
            | MetadataChange::Nullability { column, .. }
            | MetadataChange::Default { column, .. }
            | MetadataChange::Ordinal { column, .. } => column,
            MetadataChange::ConstraintMissing { constraint }
            | MetadataChange::ConstraintExtra { constraint } => constraint.name(),
            MetadataChange::IndexMissing { index } | MetadataChange::IndexExtra { index } => {
                &index.name
            }
        }
    }

    /// Whether this change can break a reader of the target.
    ///
    /// Dropping nullability is safe for readers and hostile to writers;
    /// gaining it is the reverse. Readers are the audience here, matching how
    /// the rest of the tool classifies compatibility.
    pub fn is_breaking(&self) -> bool {
        match self {
            // A column that may now be null will surprise code that assumed
            // it never was.
            MetadataChange::Nullability { now_nullable, .. } => *now_nullable,
            // A widening is safe for readers. Treating every declared-type
            // change as breaking made a lengthened varchar fail a CI gate.
            MetadataChange::NativeType { impact, .. } => *impact != TypeImpact::Widening,
            MetadataChange::Default { .. } => false,
            MetadataChange::Ordinal { .. } => false,

            // A rule the source enforces and the target does not means the
            // target can hold data the source could not: duplicate keys, or
            // values outside a checked range. Code written against the source
            // will meet rows it does not expect.
            MetadataChange::ConstraintMissing { .. } => true,

            // The reverse tightens the target. That breaks writers, not
            // readers, and readers are the audience the rest of this tool
            // classifies for — same reasoning as nullability above.
            MetadataChange::ConstraintExtra { .. } => false,

            // Indexes are a performance property, not a correctness one. A
            // missing index is worth reporting and is never a wrong answer.
            MetadataChange::IndexMissing { .. } | MetadataChange::IndexExtra { .. } => false,
        }
    }
}

impl fmt::Display for MetadataChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MetadataChange::NativeType {
                column,
                from,
                to,
                impact,
            } => {
                let note = match impact {
                    TypeImpact::Widening => " (widening)",
                    TypeImpact::Narrowing => " (narrowing)",
                    TypeImpact::Unknown => "",
                };
                write!(f, "{column}: {from} -> {to}{note}")
            }
            MetadataChange::Nullability {
                column,
                now_nullable,
            } => {
                if *now_nullable {
                    write!(f, "{column}: NOT NULL dropped")
                } else {
                    write!(f, "{column}: NOT NULL added")
                }
            }
            MetadataChange::Default { column, from, to } => {
                let render = |value: &Option<String>| {
                    value.clone().unwrap_or_else(|| "none".to_string())
                };
                write!(f, "{column}: default {} -> {}", render(from), render(to))
            }
            MetadataChange::Ordinal { column, from, to } => {
                write!(f, "{column}: position {from} -> {to}")
            }
            MetadataChange::ConstraintMissing { constraint } => {
                write!(f, "{constraint} missing from target")
            }
            MetadataChange::ConstraintExtra { constraint } => {
                write!(f, "{constraint} only in target")
            }
            MetadataChange::IndexMissing { index } => write!(f, "{index} missing from target"),
            MetadataChange::IndexExtra { index } => write!(f, "{index} only in target"),
        }
    }
}

/// Compare two catalogs, reporting only what DataFrame comparison cannot see.
///
/// Added and removed columns are deliberately not reported here — the existing
/// schema diff already finds those, and duplicating them would double-count.
pub fn compare(source: &TableCatalog, target: &TableCatalog) -> Vec<MetadataChange> {
    let target_columns = target.by_name();
    let mut changes = Vec::new();

    for column in &source.columns {
        let Some(other) = target_columns.get(column.name.as_str()) else {
            continue;
        };

        if normalize_type(&column.data_type) != normalize_type(&other.data_type) {
            changes.push(MetadataChange::NativeType {
                column: column.name.clone(),
                from: column.data_type.clone(),
                to: other.data_type.clone(),
                impact: classify_type_change(&column.data_type, &other.data_type),
            });
        }

        if column.nullable != other.nullable {
            changes.push(MetadataChange::Nullability {
                column: column.name.clone(),
                now_nullable: other.nullable,
            });
        }

        if normalize_default(&column.default) != normalize_default(&other.default) {
            changes.push(MetadataChange::Default {
                column: column.name.clone(),
                from: column.default.clone(),
                to: other.default.clone(),
            });
        }

        if column.ordinal != other.ordinal {
            changes.push(MetadataChange::Ordinal {
                column: column.name.clone(),
                from: column.ordinal,
                to: other.ordinal,
            });
        }
    }

    // Constraints and indexes are matched on what they do, never on what they
    // are called: see `Constraint::identity`.
    //
    // A kind neither side could read is skipped entirely. Comparing a kind one
    // connector cannot see against one that can would report every rule on the
    // readable side as missing from the other — a long list of differences
    // that are really one unread pragma. `MetadataReport::unread_constraints`
    // reports the gap instead.
    let comparable =
        |kind: ConstraintKind| -> bool { source.reads(kind) && target.reads(kind) };

    let target_constraints: BTreeSet<_> =
        target.constraints.iter().map(Constraint::identity).collect();
    for constraint in &source.constraints {
        if comparable(constraint.kind())
            && !target_constraints.contains(&constraint.identity())
        {
            changes.push(MetadataChange::ConstraintMissing {
                constraint: constraint.clone(),
            });
        }
    }

    let source_constraints: BTreeSet<_> =
        source.constraints.iter().map(Constraint::identity).collect();
    for constraint in &target.constraints {
        if comparable(constraint.kind())
            && !source_constraints.contains(&constraint.identity())
        {
            changes.push(MetadataChange::ConstraintExtra {
                constraint: constraint.clone(),
            });
        }
    }

    if comparable(ConstraintKind::Index) {
        let target_indexes: BTreeSet<_> = target.indexes.iter().map(IndexDef::identity).collect();
        for index in &source.indexes {
            if !target_indexes.contains(&index.identity()) {
                changes.push(MetadataChange::IndexMissing {
                    index: index.clone(),
                });
            }
        }

        let source_indexes: BTreeSet<_> = source.indexes.iter().map(IndexDef::identity).collect();
        for index in &target.indexes {
            if !source_indexes.contains(&index.identity()) {
                changes.push(MetadataChange::IndexExtra {
                    index: index.clone(),
                });
            }
        }
    }

    changes.sort_by(|a, b| a.subject().cmp(b.subject()));
    changes
}

/// Fold away spellings of the same rule.
///
/// Engines re-render a `CHECK` body into their own normal form and are
/// inconsistent about parentheses, whitespace and case, so the text that comes
/// back is rarely the text that went in.
fn normalize_expression(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .to_ascii_lowercase()
        .replace(' ', "")
}

/// Fold away spelling differences that are not real type changes.
///
/// Catalogs are inconsistent about whitespace and case, and comparing raw
/// strings would report `CHARACTER VARYING(50)` against
/// `character varying(50)` as a change.
fn normalize_type(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace(' ', "")
}

/// Treat an absent default and an empty one alike, and ignore case.
fn normalize_default(raw: &Option<String>) -> Option<String> {
    raw.as_ref()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, data_type: &str, nullable: bool, default: Option<&str>) -> ColumnDef {
        ColumnDef {
            name: name.to_string(),
            data_type: data_type.to_string(),
            nullable,
            default: default.map(str::to_string),
            ordinal: 1,
        }
    }

    fn catalog(columns: Vec<ColumnDef>) -> TableCatalog {
        TableCatalog::new(columns)
    }

    #[test]
    fn identical_catalogs_report_nothing() {
        let one = catalog(vec![column("id", "bigint", false, None)]);
        assert!(compare(&one, &one).is_empty());
    }

    #[test]
    fn a_native_type_change_polars_cannot_see_is_reported() {
        // Both load as a Polars String. Only the catalog knows they differ,
        // and this is the single biggest thing catalog reading buys.
        let source = catalog(vec![column("name", "character varying(50)", true, None)]);
        let target = catalog(vec![column("name", "text", true, None)]);

        let changes = compare(&source, &target);
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert!(matches!(changes[0], MetadataChange::NativeType { .. }));
        assert!(changes[0].is_breaking());
    }

    #[test]
    fn type_spelling_differences_are_not_changes() {
        let source = catalog(vec![column("name", "CHARACTER VARYING(50)", true, None)]);
        let target = catalog(vec![column("name", "character varying(50)", true, None)]);
        assert!(compare(&source, &target).is_empty());
    }

    #[test]
    fn dropping_not_null_is_breaking_for_readers() {
        // A column that may now be null breaks code that assumed it never was.
        let source = catalog(vec![column("email", "text", false, None)]);
        let target = catalog(vec![column("email", "text", true, None)]);

        let changes = compare(&source, &target);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].is_breaking(), "{:?}", changes[0]);
        assert_eq!(changes[0].to_string(), "email: NOT NULL dropped");
    }

    #[test]
    fn adding_not_null_is_reported_but_not_reader_breaking() {
        let source = catalog(vec![column("email", "text", true, None)]);
        let target = catalog(vec![column("email", "text", false, None)]);

        let changes = compare(&source, &target);
        assert_eq!(changes.len(), 1);
        assert!(!changes[0].is_breaking());
        assert_eq!(changes[0].to_string(), "email: NOT NULL added");
    }

    #[test]
    fn default_changes_are_reported_in_both_directions() {
        let added = compare(
            &catalog(vec![column("n", "integer", true, None)]),
            &catalog(vec![column("n", "integer", true, Some("0"))]),
        );
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].to_string(), "n: default none -> 0");

        let removed = compare(
            &catalog(vec![column("n", "integer", true, Some("0"))]),
            &catalog(vec![column("n", "integer", true, None)]),
        );
        assert_eq!(removed[0].to_string(), "n: default 0 -> none");
    }

    #[test]
    fn an_empty_default_is_the_same_as_none() {
        let source = catalog(vec![column("n", "integer", true, None)]);
        let target = catalog(vec![column("n", "integer", true, Some("   "))]);
        assert!(compare(&source, &target).is_empty());
    }

    #[test]
    fn column_reordering_is_reported() {
        let mut moved = column("b", "text", true, None);
        moved.ordinal = 2;
        let source = catalog(vec![column("b", "text", true, None)]);
        let target = catalog(vec![moved]);

        let changes = compare(&source, &target);
        assert_eq!(changes.len(), 1);
        assert!(!changes[0].is_breaking(), "reordering breaks no reader by name");
        assert_eq!(changes[0].to_string(), "b: position 1 -> 2");
    }

    #[test]
    fn added_and_removed_columns_are_left_to_the_existing_diff() {
        // Reporting them here as well would double-count them in the output.
        let source = catalog(vec![column("gone", "text", true, None)]);
        let target = catalog(vec![column("fresh", "text", true, None)]);
        assert!(compare(&source, &target).is_empty());
    }

    #[test]
    fn several_changes_to_one_column_are_all_reported() {
        let source = catalog(vec![column("c", "character varying(10)", false, None)]);
        let target = catalog(vec![column("c", "text", true, Some("''"))]);

        let changes = compare(&source, &target);
        assert_eq!(changes.len(), 3, "type, nullability and default: {changes:?}");
        assert!(changes.iter().all(|change| change.subject() == "c"));
    }


    // ---- native type impact ----

    #[test]
    fn a_longer_string_is_a_widening() {
        // The false positive that motivated this: reported as breaking, it made
        // a whole comparison backward-incompatible and would fail a CI gate
        // that should have passed.
        assert_eq!(
            classify_type_change("varchar(50)", "varchar(200)"),
            TypeImpact::Widening
        );
        assert_eq!(
            classify_type_change("nvarchar(100)", "nvarchar(4000)"),
            TypeImpact::Widening
        );
    }

    #[test]
    fn a_shorter_string_is_a_narrowing() {
        assert_eq!(
            classify_type_change("varchar(200)", "varchar(50)"),
            TypeImpact::Narrowing
        );
    }

    #[test]
    fn unbounded_is_larger_than_any_length() {
        assert_eq!(
            classify_type_change("varchar(50)", "varchar(max)"),
            TypeImpact::Widening
        );
        assert_eq!(
            classify_type_change("varchar(max)", "varchar(50)"),
            TypeImpact::Narrowing
        );
    }

    #[test]
    fn more_precision_at_the_same_scale_is_a_widening() {
        assert_eq!(
            classify_type_change("decimal(12,4)", "decimal(18,4)"),
            TypeImpact::Widening
        );
    }

    #[test]
    fn losing_scale_is_narrowing_even_when_precision_grows() {
        // decimal(18,2) holds more digits but fewer decimals than
        // decimal(12,4), so it can still lose data.
        assert_eq!(
            classify_type_change("decimal(12,4)", "decimal(18,2)"),
            TypeImpact::Narrowing
        );
    }

    #[test]
    fn a_different_base_type_is_not_classified() {
        // varchar to text is a widening in practice, but knowing that requires
        // per-engine type families. Unknown is treated as breaking, which errs
        // towards warning rather than towards silence.
        assert_eq!(
            classify_type_change("character varying(50)", "text"),
            TypeImpact::Unknown
        );
        assert_eq!(classify_type_change("int", "bigint"), TypeImpact::Unknown);
    }

    #[test]
    fn spelling_and_spacing_do_not_affect_classification() {
        assert_eq!(
            classify_type_change("VARCHAR(50)", "varchar( 200 )"),
            TypeImpact::Widening
        );
    }

    #[test]
    fn an_unparseable_parameter_falls_back_to_unknown() {
        assert_eq!(
            classify_type_change("enum('a','b')", "enum('a','b','c')"),
            TypeImpact::Unknown
        );
    }

    #[test]
    fn only_a_narrowing_or_an_unclassified_change_is_breaking() {
        let change = |from: &str, to: &str| MetadataChange::NativeType {
            column: "c".to_string(),
            from: from.to_string(),
            to: to.to_string(),
            impact: classify_type_change(from, to),
        };

        assert!(!change("varchar(50)", "varchar(200)").is_breaking());
        assert!(change("varchar(200)", "varchar(50)").is_breaking());
        assert!(change("varchar(50)", "text").is_breaking());
    }

    #[test]
    fn a_widening_says_so_when_displayed() {
        let widened = MetadataChange::NativeType {
            column: "name".to_string(),
            from: "varchar(50)".to_string(),
            to: "varchar(200)".to_string(),
            impact: TypeImpact::Widening,
        };
        assert_eq!(widened.to_string(), "name: varchar(50) -> varchar(200) (widening)");
    }


    #[test]
    fn every_availability_variant_serialises() {
        // Internally-tagged enums cannot serialise newtype variants holding a
        // plain string. If that is what these are, the JSON export panics the
        // first time a catalog read fails in the field.
        for availability in [
            CatalogAvailability::Available(TableCatalog::default()),
            CatalogAvailability::NotADatabase,
            CatalogAvailability::QueryNotATable,
            CatalogAvailability::Failed { reason: "permission denied".into() },
            CatalogAvailability::UnsupportedEngine { engine: "MySQL".to_string() },
            CatalogAvailability::NotRequested,
        ] {
            let json = serde_json::to_string(&availability);
            assert!(json.is_ok(), "{availability:?} did not serialise: {:?}", json.err());
        }
    }

    // ---- availability ----

    #[test]
    fn missing_metadata_explains_itself() {
        // The point of modelling this rather than using Option: "nothing
        // changed" and "nothing was checked" must not look alike.
        assert!(CatalogAvailability::NotADatabase
            .explain()
            .unwrap()
            .contains("no catalog"));
        assert!(CatalogAvailability::QueryNotATable
            .explain()
            .unwrap()
            .contains("SELECT"));
        assert!(CatalogAvailability::Failed { reason: "permission denied".into() }
            .explain()
            .unwrap()
            .contains("permission denied"));
    }

    #[test]
    fn an_available_catalog_has_nothing_to_explain() {
        let available = CatalogAvailability::Available(TableCatalog::default());
        assert!(available.explain().is_none());
        assert!(available.is_available());
        assert!(available.catalog().is_some());
    }

    #[test]
    fn every_reason_for_absence_is_distinguishable() {
        // Six different situations, and none may be allowed to read as
        // "nothing changed". Conflating any two of them is the bug this whole
        // enum exists to prevent.
        let reasons = [
            CatalogAvailability::NotADatabase,
            CatalogAvailability::QueryNotATable,
            CatalogAvailability::Failed { reason: "timeout".into() },
            CatalogAvailability::UnsupportedEngine { engine: "MySQL".to_string() },
            CatalogAvailability::NotRequested,
        ];
        let explanations: Vec<String> =
            reasons.iter().map(|r| r.explain().unwrap()).collect();

        let unique: std::collections::BTreeSet<&String> = explanations.iter().collect();
        assert_eq!(unique.len(), explanations.len(), "{explanations:?}");
        assert!(reasons.iter().all(|r| !r.is_available()));
    }

    #[test]
    fn an_unimplemented_connector_names_itself() {
        let reason = CatalogAvailability::UnsupportedEngine { engine: "MySQL".to_string() }
            .explain()
            .unwrap();
        assert!(reason.contains("MySQL"), "{reason}");
        assert!(reason.contains("not implemented"), "{reason}");
    }

    #[test]
    fn a_failure_is_never_mistaken_for_an_absent_catalog() {
        let failed = CatalogAvailability::Failed { reason: "timeout".into() };
        assert!(!failed.is_available());
        assert!(failed.catalog().is_none());
        assert!(failed.explain().is_some(), "the reason must survive");
    }

    // ---- constraints and indexes ------------------------------------------

    fn pk(name: &str, columns: &[&str]) -> Constraint {
        Constraint::PrimaryKey {
            name: name.to_string(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn unique(name: &str, columns: &[&str]) -> Constraint {
        Constraint::Unique {
            name: name.to_string(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn check(name: &str, expression: &str) -> Constraint {
        Constraint::Check {
            name: name.to_string(),
            expression: expression.to_string(),
        }
    }

    fn index(name: &str, columns: &[&str], unique: bool) -> IndexDef {
        IndexDef {
            name: name.to_string(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            unique,
        }
    }

    fn col(name: &str, ordinal: u32) -> ColumnDef {
        ColumnDef {
            name: name.to_string(),
            data_type: "bigint".to_string(),
            nullable: false,
            default: None,
            ordinal,
        }
    }

    fn one_column() -> Vec<ColumnDef> {
        vec![col("id", 1)]
    }

    #[test]
    fn the_same_key_under_different_names_is_not_a_difference() {
        // Engines generate constraint names — `customers_pkey` on PostgreSQL,
        // `PK__customer__3213E83F` on SQL Server. Matching on name would report
        // a difference on essentially every cross-engine comparison.
        let source =
            TableCatalog::new(one_column()).with_constraints(vec![pk("customers_pkey", &["id"])]);
        let target = TableCatalog::new(one_column())
            .with_constraints(vec![pk("PK__customer__3213E83F", &["id"])]);

        assert!(compare(&source, &target).is_empty());
    }

    #[test]
    fn key_column_order_is_part_of_the_key() {
        // (a, b) and (b, a) forbid the same duplicates but serve different
        // queries, so they are not the same constraint.
        let columns = vec![col("a", 1), col("b", 2)];
        let source = TableCatalog::new(columns.clone())
            .with_constraints(vec![pk("k", &["a", "b"])]);
        let target =
            TableCatalog::new(columns).with_constraints(vec![pk("k", &["b", "a"])]);

        assert_eq!(compare(&source, &target).len(), 2, "one missing, one extra");
    }

    #[test]
    fn a_rule_the_target_does_not_enforce_is_breaking() {
        let source = TableCatalog::new(one_column())
            .with_constraints(vec![unique("u", &["id"])]);
        let target = TableCatalog::new(one_column());

        let changes = compare(&source, &target);
        assert_eq!(changes.len(), 1);
        assert!(
            matches!(changes[0], MetadataChange::ConstraintMissing { .. }),
            "{changes:?}"
        );
        assert!(
            changes[0].is_breaking(),
            "the target can now hold duplicates the source could not"
        );
    }

    #[test]
    fn a_rule_only_the_target_enforces_is_reported_but_not_breaking() {
        // A tighter target breaks writers, not readers, and readers are the
        // audience the rest of the tool classifies for.
        let source = TableCatalog::new(one_column());
        let target = TableCatalog::new(one_column())
            .with_constraints(vec![unique("u", &["id"])]);

        let changes = compare(&source, &target);
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0], MetadataChange::ConstraintExtra { .. }));
        assert!(!changes[0].is_breaking());
    }

    #[test]
    fn indexes_are_reported_but_never_breaking() {
        let source =
            TableCatalog::new(one_column()).with_indexes(vec![index("i", &["id"], false)]);
        let target = TableCatalog::new(one_column());

        let changes = compare(&source, &target);
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0], MetadataChange::IndexMissing { .. }));
        assert!(
            !changes[0].is_breaking(),
            "a missing index is slow, not wrong"
        );
    }

    #[test]
    fn a_check_is_matched_on_its_rule_not_its_spelling() {
        // Engines re-render a CHECK body into their own normal form, so the
        // text that comes back is rarely the text that went in.
        let source = TableCatalog::new(one_column())
            .with_constraints(vec![check("a", "(amount > 0)")]);
        let target = TableCatalog::new(one_column())
            .with_constraints(vec![check("b", "AMOUNT  >  0")]);

        assert!(compare(&source, &target).is_empty());
    }

    #[test]
    fn a_kind_the_connector_cannot_read_is_skipped_not_reported_as_missing() {
        // The whole reason `unread` exists. SQLite cannot report checks; if
        // that were treated as "has none", every check on the other side would
        // be reported as missing and a migration would try to drop rules that
        // are really still there.
        let source = TableCatalog::new(one_column()).with_constraints(vec![
            check("a", "amount > 0"),
            unique("u", &["id"]),
        ]);
        let target = TableCatalog::new(one_column())
            .with_constraints(vec![unique("u2", &["id"])])
            .with_unread(vec![ConstraintKind::Check]);

        let changes = compare(&source, &target);
        assert!(
            changes.is_empty(),
            "the unique matches and the check is not comparable: {changes:?}"
        );
    }

    #[test]
    fn an_unread_kind_only_silences_that_kind() {
        let source = TableCatalog::new(one_column()).with_constraints(vec![
            check("a", "amount > 0"),
            unique("u", &["id"]),
        ]);
        let target = TableCatalog::new(one_column()).with_unread(vec![ConstraintKind::Check]);

        let changes = compare(&source, &target);
        assert_eq!(changes.len(), 1, "the unique is still compared: {changes:?}");
        assert!(matches!(
            changes[0],
            MetadataChange::ConstraintMissing {
                constraint: Constraint::Unique { .. }
            }
        ));
    }

    #[test]
    fn every_metadata_change_variant_serialises() {
        // The tagged representation cannot encode a newtype variant holding a
        // plain string, and that failure only shows on export. Constraints and
        // indexes added four variants, so this covers them too.
        let changes = vec![
            MetadataChange::ConstraintMissing {
                constraint: pk("k", &["id"]),
            },
            MetadataChange::ConstraintExtra {
                constraint: check("c", "amount > 0"),
            },
            MetadataChange::IndexMissing {
                index: index("i", &["id"], true),
            },
            MetadataChange::IndexExtra {
                index: index("j", &["id"], false),
            },
        ];

        for change in &changes {
            serde_json::to_string(change)
                .unwrap_or_else(|e| panic!("{change:?} must serialise: {e}"));
        }
    }
}

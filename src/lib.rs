//! Compare two datasets, or two schemas, across CSV, PostgreSQL, MySQL, SQL
//! Server and SQLite, and report what differs.
//!
//! Structure comparisons live in [`schema`], row comparisons in [`data`], and
//! what the database itself declares about a table — types, nullability,
//! defaults, keys, indexes — in [`catalog`].
//!
//! # A comparison can be partial, and says so
//!
//! Reading `changes.is_empty()` as "the schemas match" is the most likely way
//! to misuse this library. An empty list can equally mean nothing was checked:
//! a CSV has no catalog, an arbitrary `SELECT` has no single table to describe,
//! and no engine exposes every kind of rule. Two types carry that distinction
//! and neither should be discarded:
//!
//! - [`catalog::CatalogAvailability`] — whether a catalog was read at all, and
//!   if not, why not.
//! - [`catalog::TableCatalog::unread`] — which kinds of rule this engine would
//!   not give up, so their absence is not read as "the table has none".
//!
//! [`schema::SchemaDiffResult::scope`] goes further and names what this tool
//! never examines on any run, whatever the catalog says.

#![deny(missing_docs)]

pub mod catalog;
pub mod cli;
pub mod connectors;
pub mod data;
pub mod schema;
pub mod sqldialect;
pub mod sqltype;

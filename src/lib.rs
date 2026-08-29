//! Bijection compares two datasets or two schemas across CSV, PostgreSQL, MySQL, SQL Server and SQLite, and reports what differs.
//!
//! Structure comparisons live in [`schema`], row comparisons in [`data`], and database-declared metadata in [`catalog`].
//!
//! A comparison can be partial. The result says so via [`catalog::CatalogAvailability`] and [`catalog::TableCatalog::unread`]. Treating `changes.is_empty()` as "the schemas match" is the most common misuse; an empty change list can mean nothing was checked.

pub mod catalog;
pub mod cli;
pub mod connectors;
pub mod data;
pub mod schema;
pub mod sqldialect;
pub mod sqltype;

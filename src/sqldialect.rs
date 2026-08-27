//! Writing SQL text for a particular engine.
//!
//! Quoting rules and type spellings, kept apart from anything that builds a
//! statement: statement generation lives in the paid crate, and the knowledge
//! of how an engine spells things belongs here where it is covered by the open
//! test suite.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    Postgres,
    MySql,
    SqlServer,
    Sqlite,
}

/// Why a canonical type cannot be written for a dialect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedType {
    pub dialect: Dialect,
    /// The canonical base that could not be rendered.
    pub base: String,
    /// Why not, phrased for someone reading an error.
    pub reason: String,
}

impl std::fmt::Display for UnsupportedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}

impl Dialect {
    /// The engine's name as a person writes it, for messages.
    pub fn name(&self) -> &'static str {
        match self {
            Dialect::Postgres => "PostgreSQL",
            Dialect::MySql => "MySQL",
            Dialect::SqlServer => "SQL Server",
            Dialect::Sqlite => "SQLite",
        }
    }

    /// Quote a single identifier — a column, a table, an index.
    ///
    /// Always quotes. An unquoted identifier is folded to lower case by
    /// PostgreSQL and may be a reserved word elsewhere.
    pub fn quote_ident(&self, name: &str) -> String {
        match self {
            Dialect::Postgres | Dialect::Sqlite => {
                // PostgreSQL and SQLite use double quotes, doubling embedded ".
                let escaped = name.replace('"', "\"\"");
                format!("\"{}\"", escaped)
            }
            Dialect::MySql => {
                // MySQL uses backticks, doubling embedded backticks.
                let escaped = name.replace('`', "``");
                format!("`{}`", escaped)
            }
            Dialect::SqlServer => {
                // SQL Server uses brackets, doubling embedded ].
                let escaped = name.replace(']', "]]");
                format!("[{}]", escaped)
            }
        }
    }

    /// Quote a table reference, which may be schema-qualified.
    ///
    /// Only a single dot with non-empty sides is split into two identifiers.
    /// Anything else is quoted whole to avoid guessing a boundary.
    pub fn quote_table(&self, name: &str) -> String {
        // Split only when there is exactly one dot and neither side is empty.
        let parts: Vec<&str> = name.split('.').collect();
        if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            let left = self.quote_ident(parts[0]);
            let right = self.quote_ident(parts[1]);
            format!("{}.{}", left, right)
        } else {
            self.quote_ident(name)
        }
    }

    /// The dialect a source speaks, or `None` for one that is not a database.
    pub fn of(config: &crate::connectors::SourceConfig) -> Option<Dialect> {
        match config {
            crate::connectors::SourceConfig::File { .. } => None,
            crate::connectors::SourceConfig::Postgres { .. } => Some(Dialect::Postgres),
            crate::connectors::SourceConfig::Mysql { .. } => Some(Dialect::MySql),
            crate::connectors::SourceConfig::SqlServer { .. } => Some(Dialect::SqlServer),
            crate::connectors::SourceConfig::Sqlite { .. } => Some(Dialect::Sqlite),
        }
    }

    /// Write a canonical type as this engine's own type text.
    pub fn render_type(
        &self,
        ty: &crate::sqltype::CanonicalType,
    ) -> Result<String, UnsupportedType> {
        if !ty.recognised {
            return Err(UnsupportedType {
                dialect: *self,
                base: ty.base.clone(),
                reason: format!(
                    "`{}` is not a type this tool recognises, so it cannot be written for {}",
                    ty.base,
                    self.name()
                ),
            });
        }

        match ty.base.as_str() {
            "integer" => {
                let name = match self {
                    Dialect::Postgres | Dialect::Sqlite => "integer",
                    Dialect::MySql | Dialect::SqlServer => "int",
                };
                Ok(name.to_string())
            }
            "bigint" => Ok("bigint".to_string()),
            "smallint" => Ok("smallint".to_string()),
            "tinyint" => match self {
                Dialect::Postgres => Err(UnsupportedType {
                    dialect: *self,
                    base: "tinyint".to_string(),
                    reason: format!("{} has no `tinyint` type", self.name()),
                }),
                _ => Ok("tinyint".to_string()),
            },
            "mediumint" => match self {
                Dialect::Postgres | Dialect::SqlServer => Err(UnsupportedType {
                    dialect: *self,
                    base: "mediumint".to_string(),
                    reason: format!("{} has no `mediumint` type", self.name()),
                }),
                _ => Ok("mediumint".to_string()),
            },
            "numeric" => {
                let name = match self {
                    Dialect::Postgres | Dialect::Sqlite => "numeric",
                    Dialect::MySql | Dialect::SqlServer => "decimal",
                };
                if ty.params.is_empty() {
                    Ok(name.to_string())
                } else {
                    let params_str = ty
                        .params
                        .iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    Ok(format!("{}({})", name, params_str))
                }
            }
            "real" => {
                let name = match self {
                    Dialect::MySql => "float",
                    _ => "real",
                };
                Ok(name.to_string())
            }
            "double precision" => {
                // SQL Server's real is 4-byte and its float is 8-byte by default.
                // So double precision renders as SQL Server float.
                let name = match self {
                    Dialect::MySql => "double",
                    Dialect::SqlServer => "float",
                    _ => "double precision",
                };
                Ok(name.to_string())
            }
            "varchar" => {
                if ty.params.is_empty() {
                    // PostgreSQL treats bare varchar as unlimited, so text is the honest rendering.
                    // MySQL and SQL Server require a length and have no unlimited varchar, so there is nothing correct to write — refuse.
                    match self {
                        Dialect::Postgres | Dialect::Sqlite => Ok("text".to_string()),
                        _ => Err(UnsupportedType {
                            dialect: *self,
                            base: "varchar".to_string(),
                            reason: format!(
                                "`varchar` needs a length on {}, and none was declared",
                                self.name()
                            ),
                        }),
                    }
                } else {
                    let p = ty.params[0];
                    if p == crate::sqltype::UNBOUNDED {
                        match self {
                            Dialect::Postgres | Dialect::Sqlite => Ok("text".to_string()),
                            Dialect::MySql => Ok("longtext".to_string()),
                            Dialect::SqlServer => Ok("varchar(max)".to_string()),
                        }
                    } else {
                        Ok(format!("varchar({})", p))
                    }
                }
            }
            "char" => {
                if ty.params.is_empty() {
                    Err(UnsupportedType {
                        dialect: *self,
                        base: "char".to_string(),
                        reason: format!(
                            "`char` needs a length on {}, and none was declared",
                            self.name()
                        ),
                    })
                } else {
                    let p = ty.params[0];
                    Ok(format!("char({})", p))
                }
            }
            "text" => {
                match self {
                    Dialect::SqlServer => {
                        // SQL Server has a text type but it is deprecated and Microsoft documents it as slated for removal.
                        // varchar(max) is the supported equivalent.
                        Ok("varchar(max)".to_string())
                    }
                    _ => Ok("text".to_string()),
                }
            }
            "nvarchar" => {
                // These are UTF-16 on SQL Server. PostgreSQL and MySQL handle encoding through the database or column charset
                // rather than the type, so there is no type to map to. Rendering plain varchar would silently change the encoding,
                // which §2.1 forbids.
                match self {
                    Dialect::Postgres | Dialect::MySql => Err(UnsupportedType {
                        dialect: *self,
                        base: "nvarchar".to_string(),
                        reason: format!("{} has no `nvarchar` type", self.name()),
                    }),
                    Dialect::SqlServer | Dialect::Sqlite => {
                        if ty.params.is_empty() {
                            Err(UnsupportedType {
                                dialect: *self,
                                base: "nvarchar".to_string(),
                                reason: format!(
                                    "`nvarchar` needs a length on {}, and none was declared",
                                    self.name()
                                ),
                            })
                        } else {
                            let p = ty.params[0];
                            if p == crate::sqltype::UNBOUNDED {
                                Ok("nvarchar(max)".to_string())
                            } else {
                                Ok(format!("nvarchar({})", p))
                            }
                        }
                    }
                }
            }
            "nchar" => match self {
                Dialect::Postgres | Dialect::MySql => Err(UnsupportedType {
                    dialect: *self,
                    base: "nchar".to_string(),
                    reason: format!("{} has no `nchar` type", self.name()),
                }),
                Dialect::SqlServer | Dialect::Sqlite => {
                    if ty.params.is_empty() {
                        Err(UnsupportedType {
                            dialect: *self,
                            base: "nchar".to_string(),
                            reason: format!(
                                "`nchar` needs a length on {}, and none was declared",
                                self.name()
                            ),
                        })
                    } else {
                        let p = ty.params[0];
                        Ok(format!("nchar({})", p))
                    }
                }
            },
            "boolean" => match self {
                Dialect::SqlServer => Ok("bit".to_string()),
                _ => Ok("boolean".to_string()),
            },
            _ => Err(UnsupportedType {
                dialect: *self,
                base: ty.base.clone(),
                reason: format!(
                    "`{}` is not supported by this version for {}",
                    ty.base,
                    self.name()
                ),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::{parse_source_uri, SourceConfig};

    #[test]
    fn every_dialect_quotes_the_way_its_server_expects() {
        assert_eq!(Dialect::Postgres.quote_ident("orders"), "\"orders\"");
        assert_eq!(Dialect::Sqlite.quote_ident("orders"), "\"orders\"");
        assert_eq!(Dialect::MySql.quote_ident("orders"), "`orders`");
        assert_eq!(Dialect::SqlServer.quote_ident("orders"), "[orders]");
    }

    #[test]
    fn an_identifier_is_always_quoted_even_when_it_looks_safe() {
        // PostgreSQL folds an unquoted identifier to lower case, so a quoted userId
        // stops matching.
        for d in [
            Dialect::Postgres,
            Dialect::MySql,
            Dialect::SqlServer,
            Dialect::Sqlite,
        ] {
            let q = d.quote_ident("id");
            assert_ne!(q, "id", "dialect {:?} must quote", d);
        }
    }

    #[test]
    fn case_survives_because_the_identifier_is_quoted() {
        assert_eq!(Dialect::Postgres.quote_ident("userId"), "\"userId\"");
    }

    #[test]
    fn an_embedded_quote_character_is_doubled_not_dropped() {
        // Without doubling, the name ends the quoted region early and the rest is
        // parsed as SQL.
        assert_eq!(Dialect::Postgres.quote_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(Dialect::MySql.quote_ident("a`b"), "`a``b`");
        assert_eq!(Dialect::SqlServer.quote_ident("a]b"), "[a]]b]");
    }

    #[test]
    fn a_bracket_dialect_only_doubles_the_closing_bracket() {
        assert_eq!(Dialect::SqlServer.quote_ident("a[b"), "[a[b]");
    }

    #[test]
    fn a_schema_qualified_table_is_two_identifiers() {
        assert_eq!(
            Dialect::Postgres.quote_table("reporting.prod"),
            "\"reporting\".\"prod\""
        );
        assert_eq!(
            Dialect::MySql.quote_table("reporting.prod"),
            "`reporting`.`prod`"
        );
        assert_eq!(
            Dialect::SqlServer.quote_table("reporting.prod"),
            "[reporting].[prod]"
        );
    }

    #[test]
    fn an_unqualified_table_is_left_as_one() {
        for d in [
            Dialect::Postgres,
            Dialect::MySql,
            Dialect::SqlServer,
            Dialect::Sqlite,
        ] {
            assert_eq!(d.quote_table("prod"), d.quote_ident("prod"));
        }
    }

    #[test]
    fn an_ambiguous_table_reference_is_not_split_apart() {
        // Guessing where the boundary falls would address a different table.
        assert_eq!(
            Dialect::Postgres.quote_table("a.b.c"),
            Dialect::Postgres.quote_ident("a.b.c")
        );
        assert_eq!(
            Dialect::Postgres.quote_table(".prod"),
            Dialect::Postgres.quote_ident(".prod")
        );
        assert_eq!(
            Dialect::Postgres.quote_table("reporting."),
            Dialect::Postgres.quote_ident("reporting.")
        );
    }

    #[test]
    fn a_file_has_no_dialect() {
        let cfg = SourceConfig::File {
            path: "data.csv".to_string(),
        };
        assert_eq!(Dialect::of(&cfg), None);
    }

    #[test]
    fn each_database_source_reports_its_own_dialect() {
        let pg = parse_source_uri("postgres://u:p@h:5432/db", Some("t")).unwrap();
        let my = parse_source_uri("mysql://u:p@h:3306/db", Some("t")).unwrap();
        let ss = parse_source_uri("sqlserver://u:p@h:1433/db", Some("t")).unwrap();
        let sq = parse_source_uri("sqlite:///some.db", Some("t")).unwrap();

        assert_eq!(Dialect::of(&pg), Some(Dialect::Postgres));
        assert_eq!(Dialect::of(&my), Some(Dialect::MySql));
        assert_eq!(Dialect::of(&ss), Some(Dialect::SqlServer));
        assert_eq!(Dialect::of(&sq), Some(Dialect::Sqlite));
    }

    #[test]
    fn each_dialect_has_a_name_a_person_would_recognise() {
        assert_eq!(Dialect::Postgres.name(), "PostgreSQL");
        assert_eq!(Dialect::MySql.name(), "MySQL");
        assert_eq!(Dialect::SqlServer.name(), "SQL Server");
        assert_eq!(Dialect::Sqlite.name(), "SQLite");
    }

    #[test]
    fn each_engine_writes_its_own_name_for_the_same_type() {
        use crate::sqltype::canonical;
        let numeric = canonical("numeric(12,2)");
        assert_eq!(
            Dialect::Postgres.render_type(&numeric).unwrap(),
            "numeric(12, 2)"
        );
        assert_eq!(
            Dialect::MySql.render_type(&numeric).unwrap(),
            "decimal(12, 2)"
        );
        let integer = canonical("integer");
        assert_eq!(Dialect::Postgres.render_type(&integer).unwrap(), "integer");
        assert_eq!(Dialect::MySql.render_type(&integer).unwrap(), "int");
    }

    #[test]
    fn a_length_is_carried_through() {
        use crate::sqltype::canonical;
        let ty = canonical("varchar(50)");
        for d in [
            Dialect::Postgres,
            Dialect::MySql,
            Dialect::SqlServer,
            Dialect::Sqlite,
        ] {
            assert_eq!(d.render_type(&ty).unwrap(), "varchar(50)");
        }
    }

    #[test]
    fn an_unbounded_string_uses_each_engines_own_spelling() {
        use crate::sqltype::canonical;
        let ty = canonical("varchar(max)");
        assert_eq!(Dialect::Postgres.render_type(&ty).unwrap(), "text");
        assert_eq!(Dialect::MySql.render_type(&ty).unwrap(), "longtext");
        assert_eq!(Dialect::SqlServer.render_type(&ty).unwrap(), "varchar(max)");
        assert_eq!(Dialect::Sqlite.render_type(&ty).unwrap(), "text");
    }

    #[test]
    fn sql_server_float_is_eight_bytes_and_real_is_four() {
        use crate::sqltype::canonical;
        // This looks inverted unless you know SQL Server's widths
        let double = canonical("double precision");
        assert_eq!(Dialect::SqlServer.render_type(&double).unwrap(), "float");
        let real = canonical("real");
        assert_eq!(Dialect::SqlServer.render_type(&real).unwrap(), "real");
    }

    #[test]
    fn deprecated_types_are_not_emitted() {
        use crate::sqltype::canonical;
        let ty = canonical("text");
        assert_eq!(Dialect::SqlServer.render_type(&ty).unwrap(), "varchar(max)");
        assert_ne!(Dialect::SqlServer.render_type(&ty).unwrap(), "text");
    }

    #[test]
    fn a_type_the_engine_lacks_is_refused_rather_than_widened() {
        use crate::sqltype::canonical;
        let ty = canonical("tinyint");
        let err = Dialect::Postgres.render_type(&ty).unwrap_err();
        assert_eq!(err.base, "tinyint");
        assert_eq!(err.dialect, Dialect::Postgres);
        // widening changes the range, and a column that quietly holds more than it should is the failure this refuses to make
        assert!(!err.to_string().contains("smallint"));
    }

    #[test]
    fn a_national_character_type_is_not_downgraded_to_ascii() {
        use crate::sqltype::canonical;
        let ty = canonical("nvarchar(50)");
        for d in [Dialect::Postgres, Dialect::MySql] {
            let err = d.render_type(&ty).unwrap_err();
            assert_eq!(err.base, "nvarchar");
            // The error should not mention varchar, only nvarchar. Check for the word with backticks to avoid substring match on "nvarchar".
            assert!(!err.to_string().contains(" `varchar`"));
        }
    }

    #[test]
    fn an_unrecognised_type_is_never_guessed_at() {
        use crate::sqltype::canonical;
        let ty = canonical("geography");
        for d in [
            Dialect::Postgres,
            Dialect::MySql,
            Dialect::SqlServer,
            Dialect::Sqlite,
        ] {
            assert!(d.render_type(&ty).is_err());
        }
    }

    #[test]
    fn a_string_with_no_length_is_refused_where_a_length_is_required() {
        use crate::sqltype::canonical;
        let ty = canonical("varchar");
        assert_eq!(Dialect::Postgres.render_type(&ty).unwrap(), "text");
        assert_eq!(Dialect::Sqlite.render_type(&ty).unwrap(), "text");
        assert!(Dialect::MySql.render_type(&ty).is_err());
        assert!(Dialect::SqlServer.render_type(&ty).is_err());
    }

    #[test]
    fn boolean_is_a_bit_on_sql_server() {
        use crate::sqltype::canonical;
        let ty = canonical("boolean");
        assert_eq!(Dialect::SqlServer.render_type(&ty).unwrap(), "bit");
        assert_eq!(Dialect::Postgres.render_type(&ty).unwrap(), "boolean");
        assert_eq!(Dialect::MySql.render_type(&ty).unwrap(), "boolean");
        assert_eq!(Dialect::Sqlite.render_type(&ty).unwrap(), "boolean");
    }

    #[test]
    fn a_refusal_says_which_engine_and_which_type() {
        use crate::sqltype::canonical;
        let ty = canonical("tinyint");
        let err = Dialect::Postgres.render_type(&ty).unwrap_err();
        assert_eq!(err.dialect, Dialect::Postgres);
        assert_eq!(err.base, "tinyint");
        assert!(err.to_string().contains("PostgreSQL"));
    }
}

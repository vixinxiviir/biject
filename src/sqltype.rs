//! Canonical type names for schema comparison.
//!
//! Turns a declared type string into a comparable form, folding spellings that
//! mean the same type on the same engine. The module is deliberately small:
//! a lookup table and a string split, with no per-engine semantics.

/// Sentinel for an unbounded length, such as SQL Server's `varchar(max)`.
pub const UNBOUNDED: u64 = u64::MAX;

/// A declared type reduced to a comparable form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalType {
    /// The canonical base name, lowercase. For a spelling in the alias table
    /// this is the canonical member; otherwise it is the cleaned input.
    pub base: String,
    /// Size or precision parameters, in the order written.
    pub params: Vec<u64>,
    /// Whether `base` came from the alias table.
    pub recognised: bool,
}

/// Reduce a declared type to its canonical form.
///
/// ```rust
/// use biject::sqltype::canonical;
/// assert_eq!(canonical("VARCHAR(50)"), canonical("character varying(50)"));
/// assert!(!canonical("geography").recognised);
/// ```
pub fn canonical(declared: &str) -> CanonicalType {
    // 1. Trim, lowercase, and collapse whitespace.
    let normalized = declared
        .trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    // 2. Split on first '(' and last ')'.
    let (base_raw, params_raw) = if let Some(open) = normalized.find('(') {
        if let Some(close) = normalized.rfind(')') {
            if open < close {
                // Whatever follows the closing paren belongs to the type, and
                // is rejoined to the head before the alias lookup. MySQL writes
                // `decimal(12,2) unsigned` and PostgreSQL writes
                // `timestamp(3) with time zone`; dropping the tail made an
                // unsigned column compare equal to a signed one, and a
                // timezone-aware column equal to a naive one.
                let head = normalized[..open].trim();
                let tail = normalized[close + 1..].trim();
                let base = if tail.is_empty() {
                    head.to_string()
                } else {
                    format!("{head} {tail}")
                };
                let params = normalized[open + 1..close].to_string();
                (base, Some(params))
            } else {
                (normalized, None)
            }
        } else {
            (normalized, None)
        }
    } else {
        (normalized, None)
    };

    // 3. Parse parameters.
    let mut params = Vec::new();
    if let Some(p) = params_raw {
        let mut ok = true;
        for part in p.split(',') {
            let part = part.trim();
            if part == "max" {
                params.push(UNBOUNDED);
            } else if let Ok(v) = part.parse::<u64>() {
                params.push(v);
            } else {
                // Unparseable parameter: discard whole list.
                ok = false;
                break;
            }
        }
        if !ok {
            params.clear();
        }
    }

    // 4. Alias lookup.
    let (canonical_base, recognised) = lookup_base(&base_raw);

    // 5. Integer types discard parameters.
    if is_integer_type(&canonical_base) {
        params.clear();
    }

    CanonicalType {
        base: canonical_base,
        params,
        recognised,
    }
}

fn lookup_base(base: &str) -> (String, bool) {
    match base {
        // Integers — parameters discarded
        "int" | "integer" | "int4" => ("integer".to_string(), true),
        "bigint" | "int8" => ("bigint".to_string(), true),
        "smallint" | "int2" => ("smallint".to_string(), true),
        "tinyint" => ("tinyint".to_string(), true),
        "mediumint" => ("mediumint".to_string(), true),

        // Exact numerics
        "numeric" | "decimal" | "dec" | "fixed" => ("numeric".to_string(), true),

        // Approximate numerics
        "real" | "float4" => ("real".to_string(), true),
        "double precision" | "double" | "float8" => ("double precision".to_string(), true),

        // Character types
        "varchar" | "character varying" => ("varchar".to_string(), true),
        "char" | "character" => ("char".to_string(), true),
        "nvarchar" | "national character varying" => ("nvarchar".to_string(), true),
        "nchar" | "national character" => ("nchar".to_string(), true),
        "text" => ("text".to_string(), true),

        // Boolean
        "boolean" | "bool" => ("boolean".to_string(), true),

        // Date and time
        "timestamp" | "timestamp without time zone" => ("timestamp".to_string(), true),
        "timestamptz" | "timestamp with time zone" => ("timestamptz".to_string(), true),
        "time" | "time without time zone" => ("time".to_string(), true),
        "timetz" | "time with time zone" => ("timetz".to_string(), true),
        "date" => ("date".to_string(), true),
        "interval" => ("interval".to_string(), true),

        // Binary and other
        "bytea" => ("bytea".to_string(), true),
        "varbinary" => ("varbinary".to_string(), true),
        "binary" => ("binary".to_string(), true),
        "uuid" => ("uuid".to_string(), true),
        "json" => ("json".to_string(), true),
        "jsonb" => ("jsonb".to_string(), true),
        "xml" => ("xml".to_string(), true),

        // Unrecognised
        _ => (base.to_string(), false),
    }
}

fn is_integer_type(base: &str) -> bool {
    matches!(
        base,
        "integer" | "bigint" | "smallint" | "tinyint" | "mediumint"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_type_spelled_differently_is_the_same_type() {
        // PostgreSQL's catalog reports `character varying`, MySQL uses `varchar`.
        // Without folding, every string column looks changed on a cross-engine compare.
        assert_eq!(canonical("VARCHAR(50)"), canonical("character varying(50)"));
        assert_eq!(canonical("int"), canonical("integer"));
        assert_eq!(canonical("decimal(12,2)"), canonical("numeric(12,2)"));
        assert_eq!(canonical("bool"), canonical("boolean"));
        assert_eq!(
            canonical("timestamp with time zone"),
            canonical("timestamptz")
        );
    }

    #[test]
    fn case_and_whitespace_do_not_change_a_type() {
        assert_eq!(
            canonical("  CHARACTER   VARYING ( 50 ) "),
            canonical("varchar(50)")
        );
    }

    #[test]
    fn an_integer_display_width_is_not_capacity() {
        // MySQL writes a display width that says nothing about what the column holds.
        assert_eq!(canonical("int(11)"), canonical("int"));
        assert!(canonical("int(11)").params.is_empty());
    }

    #[test]
    fn size_is_kept_for_types_where_it_means_something() {
        assert_eq!(canonical("varchar(50)").params, vec![50]);
        assert_eq!(canonical("numeric(12,2)").params, vec![12, 2]);
    }

    #[test]
    fn max_is_an_unbounded_size() {
        assert_eq!(canonical("varchar(max)").params, vec![UNBOUNDED]);
    }

    #[test]
    fn an_unparseable_parameter_list_is_discarded_rather_than_guessed_at() {
        // Those are values, not sizes.
        let c = canonical("enum('a','b')");
        assert_eq!(c.base, "enum");
        assert!(c.params.is_empty());
    }

    #[test]
    fn an_unknown_type_keeps_its_own_name() {
        let c = canonical("geography");
        assert!(!c.recognised);
        assert_eq!(c.base, "geography");
        assert_ne!(canonical("geography"), canonical("geometry"));
    }

    #[test]
    fn float_is_left_alone_because_its_width_depends_on_the_engine() {
        assert!(!canonical("float").recognised);
    }

    #[test]
    fn a_qualifier_after_the_size_is_part_of_the_type() {
        // MySQL reports `decimal(12,2) unsigned`. Reading only as far as the
        // closing paren made an unsigned column compare equal to a signed one,
        // which is a real change in what the column can hold.
        assert_ne!(
            canonical("decimal(12,2) unsigned"),
            canonical("decimal(12,2)")
        );
        assert_eq!(canonical("decimal(12,2) unsigned").params, vec![12, 2]);
    }

    #[test]
    fn a_precision_does_not_hide_a_time_zone() {
        // PostgreSQL writes the precision inside the name:
        // `timestamp(3) with time zone`. Stopping at the closing paren left
        // both timestamp spellings looking like a bare `timestamp`.
        assert_ne!(
            canonical("timestamp(3) with time zone"),
            canonical("timestamp(3) without time zone")
        );
        assert_eq!(canonical("timestamp(3) with time zone").base, "timestamptz");
        assert_eq!(
            canonical("timestamp(3) without time zone").base,
            "timestamp"
        );
    }

    #[test]
    fn types_that_are_not_synonyms_stay_distinct() {
        assert_ne!(canonical("text"), canonical("varchar(50)"));
        assert_ne!(canonical("datetime"), canonical("timestamp"));
        assert_ne!(canonical("tinyint"), canonical("boolean"));
    }
}

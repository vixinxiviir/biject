# Specs

Working documents handed to an implementer, one at a time. Excluded from the
published crate — they describe work not yet done and go stale the moment it is.

## How this works

1. The tech lead writes a spec here.
2. The project manager hands the implementer
   [`local-implementer-system-prompt.md`](local-implementer-system-prompt.md) as
   its system prompt, then one spec file as the first message.
3. The implementer returns a diff.
4. The tech lead reviews it against the spec, fixes anything the spec got wrong,
   and commits.

**One spec per session.** Specs are sized to leave the implementer plenty of
context for the work itself. If a spec looks like it needs more than one
sitting, it should have been two specs.

## Queue

Order matters only where "depends on" says so.

### 0.9 — open

| Spec | Status | Depends on |
| --- | --- | --- |
| [0.9a-foreign-keys-model-and-postgres](0.9a-foreign-keys-model-and-postgres.md) | Open | — |
| [0.9b-foreign-keys-remaining-engines](0.9b-foreign-keys-remaining-engines.md) | Open | 0.9a |
| [0.9c-name-what-is-not-compared](0.9c-name-what-is-not-compared.md) | Open | 0.9a, 0.9b |
| [0.9d-document-the-api-database-half](0.9d-document-the-api-database-half.md) | Open | 0.9a, 0.9b |
| [0.9e-document-the-api-command-half](0.9e-document-the-api-command-half.md) | Open | 0.9d |
| [0.9f-freeze-the-public-api](0.9f-freeze-the-public-api.md) | Open | 0.9a, 0.9b, 0.9c |

The dependency chain is real this time: 0.9a defines a type that 0.9b, 0.9c,
0.9d and 0.9f all describe or extend. Handing out 0.9d before 0.9a means
documenting `Constraint` twice.

**Hand out 0.9a first.** It is the smallest of the foreign key specs and it
decides the model the other three consume. Review it properly before 0.9b goes
out — a wrong `ForeignKey` shape costs three specs, not one.

### 0.7 and 0.8 — done, shipped as 0.8.0

0.7 was never tagged. Everything in it went out in 0.8.0 rather than as a
release nobody could install; `CHANGELOG.md` says so.

| Spec | Status | Depends on |
| --- | --- | --- |
| [0.7-canonical-type-names](0.7-canonical-type-names.md) | Done — `3f3ad84` | — |
| [0.7a-dialect-and-identifier-quoting](0.7a-dialect-and-identifier-quoting.md) | Done — `a1c3413` | — |
| [0.7b-render-common-types](0.7b-render-common-types.md) | Done — `a1c3413` | 0.7a |
| [0.7c-render-remaining-types](0.7c-render-remaining-types.md) | Done — `59aa438` | 0.7b |
| [0.8a-schema-without-downloading-rows](0.8a-schema-without-downloading-rows.md) | Done | — |
| [0.8b-policy-rules-for-constraints](0.8b-policy-rules-for-constraints.md) | Done | — |
| [0.8c-fail-on-flag](0.8c-fail-on-flag.md) | Done — `1b74292` | — |
| 0.7d — `migrate` emits MySQL and SQL Server DDL | Done — `3fe05c9`, in the paid repo | 0.7a, 0.7b, 0.7c |

### Which specs may edit README.md

0.8b, 0.8c and 0.9c add user-facing behaviour and say so explicitly. Every other
spec in the queue leaves it alone, and the standing brief's ban still applies.

## What the local implementer does and does not get

**The free crate only.** `biject-pro` is the paid half and is worked on
directly, not handed out as a spec.

Three pieces of 0.9 are therefore not in this queue and are the lead's:

- **`biject-pro` after 0.9a.** Adding a variant to `Constraint` breaks its
  exhaustive matches, and `migrate` must refuse a foreign key change outright
  rather than emit `ADD CONSTRAINT ... FOREIGN KEY` — ordering that against
  another table's DDL is a different program. It goes on the `unsupported` list,
  where the SQL Server default change already sits.
- **Cross-platform binaries.** Linux, macOS and Windows in the release workflow.
  CI work that cannot be verified from a local checkout.
- **The desktop app's scope.** It has no `batch`, no policy file and no export.
  Either it gains them or it says plainly that it is a subset — and per the 1.0
  bar, saying so is a real answer, not a cop-out.

The line is not about trust, it is about what a wrong answer costs. A defect in
the free crate produces a wrong *report*, which a person reads and can argue
with. A defect in `migrate` produces a wrong *migration script*, which somebody
runs against a database they care about. A type mapped to the wrong equivalent
on another engine would be a silent, plausible-looking data loss in a paying
customer's hands.

That also decides where the dialect knowledge lives. Everything about how an
engine spells a type or quotes a name goes in the free crate, where it is
GPL-licensed, reviewable and covered by the open test suite. Only the generation
of statements is paid.

## What 0.9 is

**A complete answer, and an API worth freezing.** Two halves.

**Foreign keys** — 0.9a, 0.9b, 0.9c. Today `biject schema` reads primary keys,
unique constraints, check constraints and indexes, and says nothing whatsoever
about foreign keys. Two tables that differ only in one compare as identical.
That is the largest remaining instance of the failure this project exists to
avoid, and it is the last one in the free crate.

Note the boundary carefully: this is about *reading and reporting* a foreign key,
not generating DDL for one. Generating it means ordering work across tables,
which stays out of scope for 1.0.

0.9c finishes the thought by saying, on every run, what a schema comparison never
looks at — triggers, generated-column expressions, collations, comments,
partitioning, grants. `CatalogAvailability` reports what could not be read on a
particular run. Nothing yet reports what the tool never reads at all.

**The API freeze** — 0.9d, 0.9e, 0.9f. 1.0 means the Rust API is stable, and
today the crate produces around two hundred `missing_docs` warnings and marks
only two of its types as extensible. 0.9e also fixes a plain defect that falls
out of the work: `biject --help` currently lists its three commands with no
descriptions at all.

## What 0.7 was

Multi-dialect output. Today `biject migrate` refuses any target that is not
PostgreSQL, because every statement it writes is PostgreSQL. Getting past that
needs three things, and the first two belong in the free crate because they are
knowledge about databases rather than migration generation:

- **A canonical type model** — done. A declared type reduced to a comparable
  form, so `VARCHAR(50)` and `CHARACTER VARYING(50)` are one type.
- **A dialect layer** — how each engine quotes an identifier, and how it spells
  each canonical type. 0.7a and 0.7b.
- **Per-dialect statement generation** — the paid half, which consumes the
  above.

## Notes from the 0.7/0.8 round

- **One report claimed a spec was complete when the file it centred on had not
  been touched.** Nothing malicious — the summary was written from intent rather
  than from the diff. Re-run the acceptance commands yourself before believing a
  report; `git diff --stat` against the spec's expected file list catches this
  in one command. The system prompt already asks for real command output, and
  that ask is the reason it was caught.
- **Two spec gaps of the same shape reached review**, both from a table that
  described ordinary values and stopped short of the sentinel: `char(max)`
  rendered as `char(18446744073709551615)`, and a bare `char` was refused
  outright. Both are now worked examples in the system prompt. **Specify the
  whole input, including the parts that look like edge cases** — the failure is
  always at the end of the input or at the value that means "absent".
- One spec assertion was unsatisfiable as written: "must not contain `varchar`",
  about a case whose correct answer is `nvarchar`. The implementer spotted it and
  adjusted correctly. Read your own assertions as string operations before
  shipping them.

## Notes from the first round

- The repository is now rustfmt-formatted and CI checks it. Before that, a spec
  asking for `cargo fmt --check` produced a diff across thirteen files when the
  feature touched three. That will not recur, but the system prompt now tells
  the implementer to stop if `cargo fmt` touches files it did not edit.
- The one real defect in the first round came from an underspecified sentence,
  not from the implementer: the spec said what to do with the text before and
  inside a type's parentheses and never said what to do with the text *after*
  them, so `decimal(12,2) unsigned` silently compared equal to `decimal(12,2)`.
  **Specify the whole input, including the parts that look like edge cases.**

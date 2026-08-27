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

| Spec | Status | Depends on |
| --- | --- | --- |
| [0.7-canonical-type-names](0.7-canonical-type-names.md) | Done — landed in `3f3ad84` | — |
| [0.7a-dialect-and-identifier-quoting](0.7a-dialect-and-identifier-quoting.md) | Ready | — |
| 0.7b — rendering a canonical type per dialect | Not written | 0.7a, canonical type names |
| 0.7c — `migrate` emits MySQL and SQL Server DDL | Not written | 0.7a, 0.7b. **Lives in `biject-pro`, the private paid repo — not yet decided whether that is handed out** |

## What 0.7 is

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

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

Order matters only where "depends on" says so. The 0.8 specs are independent of
the 0.7 ones and of each other, so any of them can be picked up at any time.

| Spec | Status | Depends on |
| --- | --- | --- |
| [0.7-canonical-type-names](0.7-canonical-type-names.md) | Done — `3f3ad84` | — |
| [0.7a-dialect-and-identifier-quoting](0.7a-dialect-and-identifier-quoting.md) | Done — `a1c3413` | — |
| [0.7b-render-common-types](0.7b-render-common-types.md) | Done — `a1c3413` | 0.7a |
| [0.7c-render-remaining-types](0.7c-render-remaining-types.md) | Done — `59aa438` | 0.7b |
| [0.8a-schema-without-downloading-rows](0.8a-schema-without-downloading-rows.md) | Done | — |
| [0.8b-policy-rules-for-constraints](0.8b-policy-rules-for-constraints.md) | Done | — |
| [0.8c-fail-on-flag](0.8c-fail-on-flag.md) | Done — `1b74292` | — |
| 0.7d — `migrate` emits MySQL and SQL Server DDL | Not specced — **not for the local implementer** | 0.7a, 0.7b, 0.7c |

### Which to hand out first

**0.8c** is the smallest and touches the least — a good first task for a fresh
context, or after a break from the codebase.

**0.8a** is the most valuable. `biject schema` currently downloads every row of
both tables to compare their columns, which makes it unusable against a large
production table. It is also the only spec in the backlog that authorises
changes under `src/connectors/`.

**0.7a → 0.7b → 0.7c** must go in that order; each builds on the last. Together
they finish the free half of multi-dialect output.

### Two specs may edit README.md

0.8b and 0.8c both add user-facing options and say so explicitly. Every other
spec in the queue leaves it alone, and the standing brief's ban still applies.

## What the local implementer does and does not get

**The free crate only.** `biject-pro` is the paid half and is worked on
directly, not handed out as a spec.

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

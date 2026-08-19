# Dependency license audit

**Audit date:** 2026-08-19
**Audited version:** biject 0.4.0
**Tool:** `cargo license --avoid-build-deps --avoid-dev-deps`

> This document is an engineering record, not legal advice. The conclusions below
> reflect a reading of the license texts and should be confirmed with counsel
> before the first commercial sale.

## Why this exists

Bijection is distributed to the public under GPL-3.0-only. A separate paid
product (`biject migrate`) is distributed under proprietary terms and links the
`biject` library.

Our own code poses no obstacle to that: a single copyright holder owns all of
it, and a copyright holder is not bound by the license they grant to others.
**Third-party dependencies are the only thing that can block proprietary
distribution.** This audit exists to confirm they do not.

## Scope

Two dependency trees, audited separately, because they have different
obligations:

| Tree | Crates | Ships in | Distributed under |
| --- | --- | --- | --- |
| Core (`biject` lib + CLI) | 432 | free CLI **and** the paid binary | GPL-3.0-only / proprietary |
| Desktop (`biject-gui`) | 686 | free desktop app only | GPL-3.0-only |

The **core tree is the gating one.** The paid binary depends on the library, not
on the Tauri stack.

Build-only and dev-only dependencies are excluded; they do not ship in binaries.

## Verdict

**No blockers. Proprietary distribution of a binary linking the core tree is
viable.**

No GPL, AGPL, SSPL, BUSL, CDDL, EUPL, or Commons Clause dependency exists in
either tree.

## Core tree — license census

| License | Crates |
| --- | ---: |
| Apache-2.0 OR MIT | 268 |
| MIT | 88 |
| Unicode-3.0 | 18 |
| Apache-2.0 OR Apache-2.0 WITH LLVM-exception OR MIT | 16 |
| MIT OR Unlicense | 7 |
| Apache-2.0 OR MIT OR Zlib | 7 |
| BSD-3-Clause | 4 |
| Apache-2.0 | 4 |
| MPL-2.0 | 2 |
| BSD-3-Clause OR MIT | 2 |
| Apache-2.0 OR LGPL-2.1-or-later OR MIT | 2 |
| Apache-2.0 OR BSL-1.0 OR MIT | 2 |
| Apache-2.0 OR BSD-2-Clause OR MIT | 2 |
| Apache-2.0 AND MIT | 2 |
| Others (single crates, all permissive) | 6 |

Roughly 99% of the tree is permissive (MIT / Apache-2.0 / BSD / Zlib / Unicode /
BSL-1.0 / Unlicense). Those impose attribution obligations only.

## Items requiring attention

### 1. Three MPL-2.0 dependencies — obligation, not blocker

| Crate | Version | License | Reached via |
| --- | --- | --- | --- |
| `colored` | 2.2.0 | MPL-2.0 | **direct dependency** |
| `option-ext` | 0.2.0 | MPL-2.0 | `dirs` → `dirs-sys` |
| `smartstring` | 1.0.1 | MPL-2.0+ | `polars` → `polars-core` |

MPL-2.0 is **file-level (weak) copyleft**. Unlike the GPL it does not extend to
the combined work. MPL-2.0 §3.3 expressly permits distributing a Larger Work
under different terms, including proprietary terms, provided the MPL-covered
files themselves remain under the MPL.

Practical obligations, given we do not modify any of these crates:

- Note their use and license in `THIRD-PARTY-NOTICES.txt`.
- Tell recipients where to obtain the MPL-covered source (an upstream URL is
  sufficient; we are not modifying the files, so there is no modified source to
  publish).

**Action:** no code change needed. Ensure the notices file covers all three.

If you would prefer zero copyleft in the core tree, `colored` is the only one
you control directly — it is a direct dependency and has permissive
alternatives. The other two arrive through `dirs` and `polars` and are not
practically removable. Not recommended as a priority; the obligation is light.

### 2. `r-efi` — disjunctive, no action

Listed as `Apache-2.0 OR LGPL-2.1-or-later OR MIT`. `OR` means we select the
terms; take Apache-2.0 or MIT. The LGPL option is irrelevant. This crate targets
UEFI and is not present in Linux, macOS, or Windows builds.

### 3. Desktop tree MPL crates — no action

The Tauri stack adds `cssparser`, `cssparser-macros`, `dtoa-short`, and
`selectors` (all MPL-2.0, from the Servo project). The desktop app is
distributed **only** under GPL-3.0-only, and MPL-2.0 is explicitly
GPL-compatible. Attribution in the notices file is the only obligation.

These crates do **not** appear in the core tree and therefore do not affect the
paid binary.

## Attribution requirements for binary distribution

Nearly every permissive license here (MIT, Apache-2.0, BSD) requires that the
copyright notice and license text accompany binary redistribution. This applies
to the free binaries and the paid one alike.

**Action:** generate `THIRD-PARTY-NOTICES.txt` with `cargo-about` and ship it
alongside every binary artifact — GitHub releases, the AUR package, and the paid
download.

```bash
cargo install cargo-about
cargo about init
cargo about generate about.hbs > THIRD-PARTY-NOTICES.txt
```

This is not yet wired into the release workflow. It should be before the first
paid release.

## Reproducing this audit

```bash
cargo install cargo-license
cargo license --avoid-build-deps --avoid-dev-deps -t > core-licenses.tsv
cargo license --avoid-build-deps --avoid-dev-deps -t \
  --manifest-path tauri-app/src-tauri/Cargo.toml > gui-licenses.tsv
```

To check only the license column for copyleft (a naive case-insensitive grep for
`MPL` produces false positives against the word "i**mpl**ementation" in crate
descriptions):

```bash
awk -F'\t' '$5 ~ /GPL|MPL|EUPL|CDDL|SSPL|BUSL|OSL-|CPAL/ {print $1"\t"$2"\t"$5}' core-licenses.tsv
```

## Change log

**2026-08-19, 0.4.0.** Re-run after the database connectors were changed to
produce typed columns. That added three Cargo features — `polars/timezones`,
`rusqlite/column_decltype`, and a direct `rust_decimal` dependency with
`db-tokio-postgres` — but introduced **no new crates**: `chrono-tz` and
`rust_decimal` were already in the tree transitively. The census is unchanged
at 432 crates with the same three MPL-2.0 dependencies, so the verdict above
still holds.

**2026-08-19, MySQL and SQL Server connectors.** Re-run after those two
connectors were given real column types. Added `polars` features `dtype-i8`,
`dtype-i16`, `dtype-u8` and `dtype-u16`, which gate the narrow integer types
needed to keep TINYINT and SMALLINT distinct from INT. No new crates; the
census is unchanged at 432 with the same three MPL-2.0 dependencies.

## Re-audit triggers

Re-run before any release that adds or bumps dependencies, and specifically:

- Before the first paid release.
- On any major `polars`, `tauri`, or database-driver bump.
- Whenever a new direct dependency is added — check its license *before* merging.

Copyleft dependencies (GPL, AGPL) must not be added to the core tree. This is
noted in [CONTRIBUTING.md](../CONTRIBUTING.md).

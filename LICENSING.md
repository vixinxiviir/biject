# Licensing

## The short version

Everything in this repository is free software under **GPL-3.0-only**. You can
use it, read it, modify it, and redistribute it, including commercially, as long
as you comply with the GPL.

If you cannot comply with the GPL — most commonly because you want to embed
Bijection in a proprietary product you distribute — a **commercial license** is
available. Contact <licensing@bijection.dev>.

## What is covered by which license

| Component | Where it lives | License |
| --- | --- | --- |
| `biject` library and CLI (`schema`, `data`, `batch`) | this repository | GPL-3.0-only |
| `biject-gui` desktop app | this repository, `tauri-app/` | GPL-3.0-only |
| `biject migrate` (migration and rollback generation) | separate product, not in this repository | Commercial, proprietary |

`biject migrate` is a separate paid product. It is **not** part of this
repository and is not distributed under the GPL. Buying it does not change the
license of anything here.

## Commitments

These are promises about how this project will be run, not license terms:

- **Nothing that is free today will become paid.** The library, the CLI
  (`schema`, `data`, `batch`), and the desktop app stay free under the GPL. Paid
  functionality is only ever *additional*.
- **The free tier is maintained, not abandoned.** It receives real bug fixes, not
  just security patches.
- **No feature here will be moved behind the paid product retroactively.**

## Why dual licensing works here

Every line of code in this repository is owned by a single copyright holder. A
copyright holder is not bound by the license they grant to others, so the same
code can be offered under the GPL to the public and under commercial terms to
buyers who need them.

This depends on the project retaining clear ownership of all contributed code.
That is why [CONTRIBUTING.md](CONTRIBUTING.md) asks contributors for an explicit
license grant. Without it, dual licensing becomes impossible.

## Third-party dependencies

Bijection builds on open source work by many other people. A full inventory of
dependency licenses is maintained in [docs/licensing.md](docs/licensing.md).

Binary distributions include a `THIRD-PARTY-NOTICES.txt` file listing the
licenses and attribution notices of bundled dependencies.

## Trademark

The GPL grants rights to the *code*. It does not grant rights to the project
name. See [NOTICE](NOTICE) for how the "Bijection" and "biject" names may be
used.

## Questions

- Licensing and purchasing: <licensing@bijection.dev>
- Anything else: open an issue.

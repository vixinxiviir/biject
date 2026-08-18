# Contributing to Bijection

Thanks for your interest. Bug reports, reproductions, and patches are all
welcome.

Please read the [Inbound License Grant](#inbound-license-grant) below before
sending a pull request. It is short, and it is required.

## Before you start

- **Bugs and small fixes** — open a PR directly. No need to ask first.
- **New features, new connectors, or anything that changes the CLI surface** —
  open an issue first. Bijection has a deliberately narrow scope, and it is
  kinder to say "not a fit" before you have written the code than after.
- **Security issues** — do not open a public issue. Email
  <security@bijection.dev>.

## Development

```bash
cargo build
cargo test
cargo clippy --all-targets
```

CI runs the tests and clippy on every push and pull request, and a failure
blocks releases.

Run `cargo fmt` over the code you touched. Do not reformat the whole tree —
it is not uniformly formatted yet, so `cargo fmt --check` reports pre-existing
differences that have nothing to do with your change, and a wholesale reformat
would bury it in noise.

Three clippy lints are currently allowed in CI (`too_many_arguments`,
`needless_range_loop`, `match_ref_pats`) because they already fire across
existing code. Everything else is denied, so new warnings will fail the build.

The desktop app builds separately:

```bash
cargo build --manifest-path tauri-app/src-tauri/Cargo.toml
```

Tests use fixtures and do not require a live database. If your change touches
comparison logic, it needs a test — see `examples/` for sample data.

## Pull requests

- One logical change per PR.
- Include a test for anything that changes behavior.
- Keep the diff focused. Unrelated reformatting makes review harder.
- Every commit must carry a `Signed-off-by:` line (see below).

## Inbound License Grant

Bijection is dual-licensed: it is distributed to the public under GPL-3.0-only,
and under separate commercial terms to users who cannot comply with the GPL.
See [LICENSING.md](LICENSING.md).

This is only possible if the project can license all of its code under both.
So contributions have to come with permission to do that.

**You keep your copyright.** You are not assigning it or giving it away. You are
granting a license, and you remain free to use your own contribution however you
like, including in other projects.

By submitting a contribution, you agree to the following.

### 1. Copyright license

You grant Cody Byers and the Bijection project a perpetual, worldwide,
non-exclusive, royalty-free, irrevocable copyright license to reproduce,
prepare derivative works of, publicly display, publicly perform, sublicense,
and distribute your contribution and such derivative works, **under any license
terms, including both open source and proprietary terms**.

### 2. Patent license

You grant a perpetual, worldwide, non-exclusive, royalty-free, irrevocable
patent license to make, have made, use, offer to sell, sell, import, and
otherwise transfer your contribution, covering only those patent claims
licensable by you that are necessarily infringed by your contribution alone or
by combination of your contribution with the project.

If you institute patent litigation alleging that the project or a contribution
constitutes patent infringement, the patent licenses granted to you under this
document terminate as of the date such litigation is filed.

### 3. Your representations

You represent that:

- Each contribution is your original creation, or you have the right to submit
  it under the terms above.
- If your employer has rights to intellectual property you create, you have
  received permission to make the contribution on their behalf, or your employer
  has waived those rights.
- Your contribution does not knowingly include third-party code under terms
  incompatible with this grant. If it includes third-party material, you have
  identified it in your pull request along with its license.

### 4. No obligation and no warranty

You are not required to provide support for your contribution. Except for the
representations above, your contribution is provided "AS IS", without warranty
of any kind.

The project is under no obligation to accept, merge, or continue to use any
contribution.

### How you agree

Add a `Signed-off-by:` line to every commit, using your real name and an email
address you control:

```
Signed-off-by: Jane Doe <jane@example.com>
```

`git commit -s` adds it for you.

That sign-off certifies that you have read this section and agree to the grant
above for that contribution.

Pull requests without sign-off cannot be merged. If you would rather not agree
to this, that is completely fine — please open an issue describing the change
instead, and it can be implemented independently.

## Third-party code

Do not paste code from another project into a PR, even a permissively licensed
one, without saying so. If a dependency would solve the problem better than
vendored code, propose the dependency.

New dependencies are evaluated on license as well as merit. Copyleft
dependencies (GPL, AGPL) cannot be added. See [docs/licensing.md](docs/licensing.md).

# Contributing to Bijection

Thanks for your interest. Bug reports, reproductions, and patches are all
welcome.

Please read the [Contributor License Agreement](#contributor-license-agreement)
below before sending a pull request. It is required.

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

## Third-party code

Do not paste code from another project into a PR, even a permissively licensed
one, without saying so. If a dependency would solve the problem better than
vendored code, propose the dependency.

New dependencies are evaluated on license as well as merit. Copyleft
dependencies (GPL, AGPL) cannot be added. See [docs/licensing.md](docs/licensing.md).

## Contributor License Agreement

### In plain English, before the legal text

Bijection is dual-licensed. It is distributed to the public under GPL-3.0-only,
and separately under commercial terms to users who cannot comply with the GPL.
See [LICENSING.md](LICENSING.md).

That only works if the project has permission to license all of its code both
ways. So contributions need to come with that permission.

Three things worth knowing:

- **You keep your copyright.** Section 2.1(a) says so explicitly. You are not
  assigning anything, and you remain free to use your own contribution however
  you like, including in other projects.
- **Your contribution stays free software, permanently.** Section 2.3 lets us
  license your work commercially, but only on the condition that we *also* keep
  licensing it under the license the project was using when you contributed. We
  cannot take your contribution proprietary-only. That is a binding condition,
  not a promise.
- **Sections 4 and 5 protect you.** They disclaim warranties and liability that
  you would otherwise owe us for contributing.

If you would rather not agree to this, that is completely fine. Please open an
issue describing the change instead, and it can be implemented independently.

### The agreement

> Adapted from the Harmony Individual Contributor License Agreement (HA-CLA-I)
> Version 1.0, dated July 4, 2011, which is licensed under a
> [Creative Commons Attribution 3.0 Unported License](https://creativecommons.org/licenses/by/3.0/).
> Section 2.3 uses Harmony's Option Five. Placeholders have been filled in and
> the signature block replaced with the sign-off procedure described below. The
> operative terms are otherwise unaltered.

Thank you for your interest in contributing to Bijection ("We" or "Us").

This contributor agreement ("Agreement") documents the rights granted by
contributors to Us. "We" and "Us" mean Cody Byers, the copyright holder of
Bijection. To make this document effective, please agree to it by following the
instructions in [How you agree](#how-you-agree) below. This is a legally binding
document, so please read it carefully before agreeing to it. The Agreement may
cover more than one software project managed by Us.

#### 1. Definitions

"You" means the individual who Submits a Contribution to Us.

"Contribution" means any work of authorship that is Submitted by You to Us in
which You own or assert ownership of the Copyright. If You do not own the
Copyright in the entire work of authorship, please follow the instructions in
[Contributions you do not wholly own](#contributions-you-do-not-wholly-own).

"Copyright" means all rights protecting works of authorship owned or controlled
by You, including copyright, moral and neighboring rights, as appropriate, for
the full term of their existence including any extensions by You.

"Material" means the work of authorship which is made available by Us to third
parties. When this Agreement covers more than one software project, the Material
means the work of authorship to which the Contribution was Submitted. After You
Submit the Contribution, it may be included in the Material.

"Submit" means any form of electronic, verbal, or written communication sent to
Us or our representatives, including but not limited to electronic mailing
lists, source code control systems, and issue tracking systems that are managed
by, or on behalf of, Us for the purpose of discussing and improving the
Material, but excluding communication that is conspicuously marked or otherwise
designated in writing by You as "Not a Contribution."

"Submission Date" means the date on which You Submit a Contribution to Us.

"Effective Date" means the date You execute this Agreement or the date You first
Submit a Contribution to Us, whichever is earlier.

"Media" means any portion of a Contribution which is not software.

#### 2. Grant of Rights

**2.1 Copyright License**

(a) You retain ownership of the Copyright in Your Contribution and have the same
rights to use or license the Contribution which You would have had without
entering into the Agreement.

(b) To the maximum extent permitted by the relevant law, You grant to Us a
perpetual, worldwide, non-exclusive, transferable, royalty-free, irrevocable
license under the Copyright covering the Contribution, with the right to
sublicense such rights through multiple tiers of sublicensees, to reproduce,
modify, display, perform and distribute the Contribution as part of the
Material; provided that this license is conditioned upon compliance with
Section 2.3.

**2.2 Patent License**

For patent claims including, without limitation, method, process, and apparatus
claims which You own, control or have the right to grant, now or in the future,
You grant to Us a perpetual, worldwide, non-exclusive, transferable,
royalty-free, irrevocable patent license, with the right to sublicense these
rights to multiple tiers of sublicensees, to make, have made, use, sell, offer
for sale, import and otherwise transfer the Contribution and the Contribution in
combination with the Material (and portions of such combination). This license
is granted only to the extent that the exercise of the licensed rights infringes
such patent claims; and provided that this license is conditioned upon
compliance with Section 2.3.

**2.3 Outbound License**

Based on the grant of rights in Sections 2.1 and 2.2, if We include Your
Contribution in a Material, We may license the Contribution under any license,
including copyleft, permissive, commercial, or proprietary licenses. As a
condition on the exercise of this right, We agree to also license the
Contribution under the terms of the license or licenses which We are using for
the Material on the Submission Date.

In addition, We may use the following licenses for Media in the Contribution:
GPL-3.0-only (including any right to adopt any future version of a license if
permitted).

**2.4 Moral Rights.** If moral rights apply to the Contribution, to the maximum
extent permitted by law, You waive and agree not to assert such moral rights
against Us or our successors in interest, or any of our licensees, either direct
or indirect.

**2.5 Our Rights.** You acknowledge that We are not obligated to use Your
Contribution as part of the Material and may decide to include any Contribution
We consider appropriate.

**2.6 Reservation of Rights.** Any rights not expressly licensed under this
section are expressly reserved by You.

#### 3. Agreement

You confirm that:

(a) You have the legal authority to enter into this Agreement.

(b) You own the Copyright and patent claims covering the Contribution which are
required to grant the rights under Section 2.

(c) The grant of rights under Section 2 does not violate any grant of rights
which You have made to third parties, including Your employer. If You are an
employee, You have had Your employer approve this Agreement or sign the Entity
version of this document. If You are less than eighteen years old, please have
Your parents or guardian sign the Agreement.

(d) You have followed the instructions in
[Contributions you do not wholly own](#contributions-you-do-not-wholly-own), if
You do not own the Copyright in the entire work of authorship Submitted.

#### 4. Disclaimer

EXCEPT FOR THE EXPRESS WARRANTIES IN SECTION 3, THE CONTRIBUTION IS PROVIDED "AS
IS". MORE PARTICULARLY, ALL EXPRESS OR IMPLIED WARRANTIES INCLUDING, WITHOUT
LIMITATION, ANY IMPLIED WARRANTY OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
PURPOSE AND NON-INFRINGEMENT ARE EXPRESSLY DISCLAIMED BY YOU TO US. TO THE
EXTENT THAT ANY SUCH WARRANTIES CANNOT BE DISCLAIMED, SUCH WARRANTY IS LIMITED
IN DURATION TO THE MINIMUM PERIOD PERMITTED BY LAW.

#### 5. Consequential Damage Waiver

TO THE MAXIMUM EXTENT PERMITTED BY APPLICABLE LAW, IN NO EVENT WILL YOU BE
LIABLE FOR ANY LOSS OF PROFITS, LOSS OF ANTICIPATED SAVINGS, LOSS OF DATA,
INDIRECT, SPECIAL, INCIDENTAL, CONSEQUENTIAL AND EXEMPLARY DAMAGES ARISING OUT
OF THIS AGREEMENT REGARDLESS OF THE LEGAL OR EQUITABLE THEORY (CONTRACT, TORT OR
OTHERWISE) UPON WHICH THE CLAIM IS BASED.

#### 6. Miscellaneous

**6.1** This Agreement will be governed by and construed in accordance with the
laws of the State of Arizona, United States of America, excluding its conflicts
of law provisions. Under certain circumstances, the governing law in this
section might be superseded by the United Nations Convention on Contracts for
the International Sale of Goods ("UN Convention") and the parties intend to
avoid the application of the UN Convention to this Agreement and, thus, exclude
the application of the UN Convention in its entirety to this Agreement.

**6.2** This Agreement sets out the entire agreement between You and Us for Your
Contributions to Us and overrides all other agreements or understandings.

**6.3** If You or We assign the rights or obligations received through this
Agreement to a third party, as a condition of the assignment, that third party
must agree in writing to abide by all the rights and obligations in the
Agreement.

**6.4** The failure of either party to require performance by the other party of
any provision of this Agreement in one situation shall not affect the right of a
party to require such performance at any time in the future. A waiver of
performance under a provision in one situation shall not be considered a waiver
of the performance of the provision in the future or a waiver of the provision
in its entirety.

**6.5** If any provision of this Agreement is found void and unenforceable, such
provision will be replaced to the extent possible with a provision that comes
closest to the meaning of the original provision and which is enforceable. The
terms and conditions set forth in this Agreement shall apply notwithstanding any
failure of essential purpose of this Agreement or any limited remedy to the
maximum extent possible under law.

### How you agree

Add a `Signed-off-by:` line to every commit, using your real name and an email
address you control:

```
Signed-off-by: Jane Doe <jane@example.com>
```

`git commit -s` adds it for you.

That sign-off is your electronic execution of the Agreement above, for that
contribution. Pull requests without sign-off cannot be merged.

### Contributions you do not wholly own

If you do not own the copyright in the entire work you are submitting — because
it derives from another project, because someone else co-wrote it, or because
your employer holds rights in it — **do not open a pull request**. Open an issue
first, describing:

- which parts you did not write,
- where they came from,
- and the license or other restrictions that apply to them.

Employer-owned work needs your employer's approval, per Section 3(c). If an
employer wants to grant rights covering a whole team, the Harmony Entity CLA
(HA-CLA-E) is the right instrument and we can put one in place.

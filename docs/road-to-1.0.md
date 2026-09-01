# The road to 1.0

Written after 0.9.0 shipped. Five steps, in the order they should be done.
Each says what it is, why it blocks 1.0, and what proves it finished.

The bar has not changed: **the tool must not give a partial answer without
saying so.** Step 1 exists because the desktop app currently breaks it.

Nothing on the "explicitly not 1.0" list moves. Checksum and sampling
comparison, cloud warehouse connectors and cross-engine schema diff all stay
post-1.0, and the landing page keeps saying so.

---

## Step 1 — The desktop app under-reports what 0.9 added

**Do this first. It is a live defect, not a gap.**

`biject schema` on the command line reports a foreign key in full and ends every
run with what it never examines. The desktop app does neither:

- `describeConstraint` in `tauri-app/ui/index.html` handles `primary_key`,
  `unique` and `check`, then falls through to `default: return c.kind`. A
  foreign key therefore renders as the bare word **`foreign_key`** — no columns,
  no referenced table, no `ON DELETE`. The change is reported as breaking, and
  the user cannot see which key it was.
- The `scope` object added in 0.9.0 is not read at all, so the GUI never says
  what it did not look at. `grep -c "scope" tauri-app/ui/index.html` returns 0.

Both were out of scope for the specs that introduced them, deliberately. That
was the right call per change and the wrong outcome overall, which is what this
step is for.

**Done when:** the GUI renders a foreign key with its columns, referenced table,
referenced columns and both referential actions, and shows the scope footer;
and a comparison of two tables differing only in a foreign key reads the same in
both interfaces. Check it against a real PostgreSQL, not a fixture.

**Size:** small. One renderer and one section.

---

## Step 2 — Cross-platform binaries

`.github/workflows/release.yml` has two jobs and both are `runs-on:
ubuntu-latest`. Every release binary is Linux. That is untenable for a paid tool
in a field that skews to macOS, and it is the oldest item on the 1.0 list.

**What it involves:**

- A build matrix over `ubuntu-latest`, `macos-latest` and `windows-latest`, for
  the CLI and the desktop app.
- Per-platform bundles from Tauri. `tauri.conf.json` sets `"targets": "all"`,
  which means `.deb`/`.AppImage` on Linux, `.dmg`/`.app` on macOS, `.msi`/`.exe`
  on Windows. The Linux build installs system dependencies that the other two
  do not need and cannot use.
- **`keyring` needs runtime testing, and Linux is the fragile one.** Only the
  desktop app uses it — `profiles.rs` is called from `tauri-app/src-tauri` and
  from nowhere else, so **the CLI, and therefore the paid binary, never touches
  a keychain.** Where it is used, it is one API over three different system
  services: Credential Manager on Windows and Keychain on macOS are both part of
  the OS and always present, while Linux needs a Secret Service provider —
  gnome-keyring or KWallet — which a minimal or headless install may simply not
  have.

  So the platform most likely to fail is the one already shipping. A CI job
  proves it compiles; only saving a profile, closing the app, reopening it and
  loading the profile back proves it stores anything.

  The code already fails in the right direction: `save_profile` writes the
  password to the keychain *before* writing metadata to disk, so a keychain that
  is absent produces an error and no half-saved profile.
- `rusqlite` already branches on `cfg(target_os = "linux")` to pick bundled
  SQLite. Confirm the non-Linux path builds on macOS as well as Windows.
- **Building for macOS requires Apple hardware.** Rust can target
  `aarch64-apple-darwin` from anywhere, but linking needs Apple's SDK, which is
  licensed for use only on Apple-branded hardware. There is no legitimate
  cross-compile from Windows. The options are a `macos-latest` GitHub Actions
  runner — Apple hardware rented by the minute, free for a public repository —
  or a Mac. For the free crate the runner settles it; for `biject-pro` it means
  either adding CI to a private repository, where macOS minutes are billed at a
  multiplier, or owning a Mac.
- Unsigned macOS and Windows binaries will be blocked by Gatekeeper and
  SmartScreen. That is separate from building, and comes with its own price: an
  Apple Developer Program membership and a Developer ID certificate to sign and
  notarise. Saying plainly that the binaries are unsigned, and documenting the
  right-click-Open step, is a legitimate 1.0 answer. The quieter one is not.

**Done when:** a tagged build produces installable artifacts for all three, and
each has been downloaded and run on that platform — CLI `--version` and a real
comparison, plus a GUI profile saved and reloaded.

---

## Step 3 — Desktop parity, or an honest scope cut

After step 1 the GUI reports everything the CLI does about a schema. It still
does not have:

| CLI feature | In the GUI? |
| --- | --- |
| `schema`, `data` comparison | yes |
| Catalog metadata, constraints, indexes | yes |
| Connection profiles | yes, and the CLI has none |
| `batch` manifests | **no** |
| `--policy` schema contracts | **no** |
| `--output` / `--format` export | **no** |
| `--fail-on` | not applicable — it is an exit code |

**This is a decision, not a task.** Either build the three, or state in the app
and the README that the desktop app is a subset and which parts. Per the 1.0
bar, saying so is a real answer rather than a cop-out — but it has to actually
be said, in the app, not only in a changelog.

**Recommendation:** state the cut. Batch and policy are CI-shaped features and
the GUI is not where anyone runs CI. Export is the one worth reconsidering,
because someone comparing two schemas in a window plausibly wants the CSV.

**Done when:** either the features exist, or the app names what it does not do
and the README agrees with the app.

---

## Step 4 — The paid product has no release machinery at all

This is the biggest unknown and the least visible, because none of it is in the
free repository.

`biject-pro` has **no CI** — no `.github/` at all. Every build of the paid
binary so far has been `cargo build` on this Windows machine. Before anyone can
buy it:

1. **A reproducible build.** At minimum a documented command and a checksum; at
   best the same matrix as step 2, since a macOS buyer is the likely buyer.
2. **Licence issuing, exercised end to end.** `src/bin/issue.rs` exists behind
   the `issuer` feature and has never been run in anger. Issue a licence against
   the offline key, take it to a machine that has never seen the key, and run
   the paid binary with it. Then try a tampered one, and one whose version
   ceiling is below the binary — each must be refused with a message a buyer can
   act on. There is no expiry to test: the payload carries `email`, `tier`,
   `seats`, `issued` and `max_version` and nothing else, and a licence is
   perpetual for the `MAJOR.MINOR` it was bought against.
3. **The key's custody.** `secrets/signing.key` is 43 bytes, gitignored, and
   currently exists on one disk plus the NAS. Losing it invalidates every
   licence ever issued and there is no recovery. Decide where the second and
   third copies live before there is a customer.
4. **A merchant of record.** Paddle, Lemon Squeezy or FastSpring — they handle
   VAT and delivery, which is the entire reason not to build billing. Pick one,
   and check its delivery mechanism can hand over a file, since that is what a
   licence is.
5. **Delivery.** Manual until it hurts, per the roadmap. But "manual" still
   needs a written runbook: what to run, what to send, what to record.

**Done when:** you have bought your own product with a real card, received the
binary and a licence, and run a migration with it on a machine that has never
had the source.

---

## Step 5 — The 1.0 commitments

Small, and last, because it is the part that cannot be taken back.

- **Act on `docs/api-surface.md`.** It is a survey; nothing has been decided
  from it. Anything that should not be public has to become private *before*
  1.0. Read its "What stands out" section first — most of what looks unused is
  used by the paid crate, which the table cannot see.
- **Declare an MSRV.** `Cargo.toml` has no `rust-version`, so the crate claims
  to build on any Rust and is tested on whatever `stable` was that week. Pick a
  version, put it in `Cargo.toml`, and add it to the CI matrix so the claim is
  checked rather than asserted.
- **Say what 1.0 means for the API**, in the README: which modules are stable,
  that `#[non_exhaustive]` types may grow, and that the CLI's output format is
  not an API unless you say it is. `--format json` and `--format csv` almost
  certainly should be.
- **The free/paid line, restated.** `LICENSING.md` already carries the
  commitment that nothing free becomes paid. 1.0 is when people start relying
  on it.

---

## Release

Then the usual: four version places, both lockfiles, `CHANGELOG.md`, tag,
vendor tarball, `cargo publish`, AUR, and the paid channel live.

Roughly: step 1 is an evening, step 2 is a weekend of CI and a second of
platform testing, step 3 is a decision plus whatever it implies, step 4 is the
long pole and mostly not code, step 5 is an evening.

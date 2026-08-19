# Releasing to the AUR

Maintainer documentation for publishing `biject` to the Arch User Repository.

This directory is excluded from the published crate, so nothing here ships to
crates.io.

- [One-time setup](#one-time-setup) — AUR account and SSH, done once per machine
- [Before every release](#before-every-release) — upstream steps that must
  complete first
- [Per-release checklist](#per-release-checklist) — the recurring work
- [First import only](#first-import-only) — initial upload and the datadiff merge
- [Troubleshooting](#troubleshooting)

---

## One-time setup

Needed once per machine.

**Install the tooling:**

```bash
sudo pacman -S --needed base-devel git devtools pacman-contrib namcap
```

`pacman-contrib` provides `updpkgsums`, `devtools` provides clean-chroot builds,
`namcap` lints packages.

**Set a git identity.** The AUR rejects commits without a valid author:

```bash
git config --global user.email "you@example.com"
```

**Register an SSH key.** Generate one if needed:

```bash
ssh-keygen -t ed25519 -C "aur" -f ~/.ssh/aur
```

Paste `~/.ssh/aur.pub` into [aur.archlinux.org](https://aur.archlinux.org) → My
Account → SSH Public Key. The AUR authenticates by key only; there is no
password push.

If the key is not at a default path, add to `~/.ssh/config`:

```
Host aur.archlinux.org
  User aur
  IdentityFile ~/.ssh/aur
```

**Verify it works** before anything else:

```bash
ssh aur@aur.archlinux.org help
```

A list of commands means you are authenticated. `Permission denied (publickey)`
means the key is not registered.

**Clone the AUR package repo:**

```bash
git clone ssh://aur@aur.archlinux.org/biject.git aur-biject
```

On a first import this repo is empty and git warns about cloning an empty
repository. That is expected.

---

## Before every release

The AUR package builds from a GitHub release, so the upstream release must
already exist and be complete.

1. **Bump the version in all four places.** These have drifted before — the
   0.2.x line ended up at 0.2.0 / 0.2.0 / 0.2.2 across three files.

   | File | Field |
   | --- | --- |
   | `Cargo.toml` | `version` |
   | `tauri-app/src-tauri/Cargo.toml` | `version` |
   | `tauri-app/src-tauri/tauri.conf.json` | `version` |
   | `packaging/aur/PKGBUILD` | `pkgver` |

   **Then regenerate both lockfiles and commit them.** The Tauri app depends on
   the CLI crate by path, so bumping the CLI version leaves
   `tauri-app/src-tauri/Cargo.lock` recording the old one. CI checks each crate
   with `--locked`, which forbids updating a stale lockfile rather than fixing
   it, so the job fails with "cannot update the lock file". This has broken CI
   twice — once on a dependency bump, once on a version bump.

   ```bash
   cargo metadata --format-version 1 > /dev/null
   ```

   ```bash
   cargo metadata --manifest-path tauri-app/src-tauri/Cargo.toml --format-version 1 > /dev/null
   ```

   Confirm both agree before pushing:

   ```bash
   grep -A1 '^name = "biject"$' Cargo.lock tauri-app/src-tauri/Cargo.lock
   ```

2. **Run `cargo update` if a dependency is holding back the build**, and verify
   against the toolchain CI uses, not just your local default. These diverge and
   that divergence has broken a release before:

   ```bash
   rustup toolchain install stable
   ```

   ```bash
   cargo +stable check --locked --all-targets
   ```

   Check the Tauri crate too — it has its own lockfile and the workflow verifies
   it as a separate job:

   ```bash
   cargo +stable check --locked --manifest-path tauri-app/src-tauri/Cargo.toml
   ```

3. **Commit, tag, and push.** The tag triggers the release workflow:

   ```bash
   git tag -a v0.3.0 -m "Bijection 0.3.0"
   ```

   ```bash
   git push origin v0.3.0
   ```

4. **Wait for the workflow** (roughly 15–25 minutes; it vendors the whole
   dependency tree) and confirm the release carries `biject-VERSION.tar.gz`,
   `biject-vendor-VERSION.tar.zst`, and `aur-sha256sums.txt`. **The AUR package
   cannot build without the vendor tarball.**

5. **Publish the crate:**

   ```bash
   cargo publish
   ```

---

## Per-release checklist

Run from inside `aur-biject`. Substitute the real version for `VERSION`.

**1. Sync the PKGBUILD** from the repo, or edit `pkgver` in place:

```bash
cp ../biject/packaging/aur/PKGBUILD .
```

Reset `pkgrel=1` on a version bump. Increment `pkgrel` instead — leaving
`pkgver` alone — when only the packaging changed and upstream did not.

**2. Update the checksums.** Never leave `SKIP` in a published package; it
disables integrity verification for your users:

```bash
updpkgsums
```

**3. Regenerate `.SRCINFO`.** Do this after *every* PKGBUILD edit. A stale
`.SRCINFO` is the most common cause of a rejected push:

```bash
makepkg --printsrcinfo > .SRCINFO
```

**4. Build in a clean chroot.** This catches missing `depends` that your own
machine already satisfies — the usual reason a package gets flagged right after
upload:

```bash
pkgctl build
```

On older devtools the equivalent is `extra-x86_64-build`. Do not run `makepkg`
as root; it refuses.

**5. Lint:**

```bash
namcap PKGBUILD *.pkg.tar.zst
```

License-file warnings are usually ignorable. Dependency warnings are not.

**6. Install and smoke-test:**

```bash
makepkg -si
```

Confirm `biject --version` matches the release and `biject --help` lists
`schema`, `data`, and `batch`.

**7. Commit and push.** Only these two files belong in an AUR repo — no
tarballs, no built packages, no `src/` or `pkg/`:

```bash
git add PKGBUILD .SRCINFO
```

```bash
git commit -m "Update to VERSION"
```

```bash
git push origin master
```

AUR repos use `master`, regardless of the branch name upstream.

**8. Verify** the package page renders correctly and install as a user would:

```bash
yay -S biject
```

---

## First import only

Already completed for 0.3.0. Kept for reference.

The initial push is the same as a normal release; the commit message is
conventionally `Initial import: biject VERSION`.

**Retiring the old `datadiff` package.** On the `datadiff` package page →
Package Actions → Submit Request → Type: **Merge** → Merge into: `biject`, with
a comment explaining the rename.

Merge rather than Deletion: a merge transfers votes and comments and redirects
the old page, while deletion discards that history. A Package Maintainer
processes it manually, so it can take days. Comment on the old package page
immediately so users are not stranded while the request sits in the queue.

Note that `replaces=` does little for AUR packages — `pacman -Syu` only honours
it for packages in synced binary repos, and helpers vary. The `conflicts=` entry
is what actually works: pacman prompts to remove `datadiff` when `biject` is
installed. The merge request is the real redirect.

---

## Troubleshooting

**Push rejected, `.SRCINFO` errors** — regenerate it. It must match the PKGBUILD
exactly, and it is validated server-side.

**`Permission denied (publickey)`** — the key is not on your AUR account, or SSH
is not offering it. Test with `ssh aur@aur.archlinux.org help`.

**Build fails on a `transmute` or similar compile error in a dependency** — a
new rustc has broken an old crate. Find the offender, bump it with
`cargo update -p CRATE`, verify against current stable, and cut a new upstream
release. Both lockfiles may need it. This is recurring maintenance while the
project pins an old `polars`.

**Vendor tarball 404** — the release workflow did not finish. Check Actions;
the `linux-artifacts` job is gated on `check` passing.

**Package flagged out-of-date** — someone noticed a new upstream tag before you
updated the PKGBUILD. Work through the per-release checklist and the flag
clears.

**Do not let the package go stale.** Flagged packages can be orphaned and
adopted by someone else. The free tier is the funnel; keeping it maintained is
the point.

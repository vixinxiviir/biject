# Migrating from datadiff to Bijection (0.2.x → 0.3.0)

The project was renamed from `datadiff` to **Bijection** in 0.3.0. The crate,
binary, and command are all `biject`.

Nothing was removed and no behavior changed. Every command works exactly as it
did before — only the names moved.

## Summary

| Was | Now |
| --- | --- |
| `datadiff` (CLI binary) | `biject` |
| `datadiff-gui` (desktop app) | `biject-gui` |
| `datadiff` (crate / library) | `biject` |
| `use datadiff::...` | `use biject::...` |
| default export basename `datadiff_export` | `biject_export` |
| config dir `<data-local>/datadiff/` | `<data-local>/biject/` |
| keychain service `datadiff` | `biject` |
| desktop bundle id `io.github.vixinxiviir.datadiff` | `io.github.vixinxiviir.biject` |

## CLI users

Replace `datadiff` with `biject` in scripts and CI:

```bash
biject schema --source dev.csv --target prod.csv
biject data   --source dev.csv --target prod.csv --key id
biject batch  --manifest pairs.json --key id
```

If you export without specifying `--output`, generated files are now named
`biject_export*` instead of `datadiff_export*`. Pass `--output` explicitly if
anything downstream depends on the old name.

## Saved connection profiles — action required

**Connection profiles and saved passwords do not carry over automatically.**

Profiles live in a directory named after the application, and passwords live in
the OS keychain under a service name that also matches. Both changed with the
rename, so 0.3.0 will not see what 0.2.x stored.

You have two options.

### Option A: move the profile file, re-enter passwords

Profile metadata (host, port, database, username) transfers by moving one file.
Passwords do not — the OS keychain entries must be recreated.

Linux:
```bash
mv ~/.local/share/datadiff ~/.local/share/biject
```

macOS:
```bash
mv ~/Library/Application\ Support/datadiff ~/Library/Application\ Support/biject
```

Windows (PowerShell):
```powershell
Move-Item "$env:LOCALAPPDATA\datadiff" "$env:LOCALAPPDATA\biject"
```

Then open `biject-gui` and re-save the password for each profile.

### Option B: recreate profiles

If you have only a couple of profiles, deleting the old directory and creating
them again in `biject-gui` is faster.

### Cleaning up old keychain entries

Old entries remain in your keychain under the service name `datadiff` and are
harmless, but you may want to remove them:

- **macOS** — Keychain Access, search `datadiff`, delete the entries.
- **Windows** — Control Panel → Credential Manager → Generic Credentials.
- **Linux** — Seahorse, or `secret-tool clear service datadiff`.

## Library users

```rust
// before
use datadiff::data;
use datadiff::schema;

// after
use biject::data;
use biject::schema;
```

Public types and function signatures are unchanged. `DataDiffError` and
`SchemaDiffError` keep their names — they describe the operation (a data diff, a
schema diff), not the old product name.

One addition: the clap command surface now lives in the library as
`biject::cli`, exposing `Cli`, `Commands`, and `dispatch`. `main.rs` is a thin
wrapper over it. Existing code is unaffected.

## Arch Linux / AUR

The `biject` package declares `replaces=('datadiff' 'datadiff-gui')`, so a normal
system upgrade swaps the package automatically:

```bash
yay -Syu
```

The old `datadiff` package is deprecated. If your helper does not pick up the
replacement, install `biject` directly and remove `datadiff`.

## Desktop app

The bundle identifier changed, so installers treat Bijection as a new
application rather than an upgrade. **Uninstall DataDiff manually** after
installing Bijection, or you will have both in your applications list.

## Repository and links

The GitHub repository moved to `github.com/vixinxiviir/biject`. GitHub redirects
the old URL, so existing clones, links, and issue references keep working. To
update a local clone's remote:

```bash
git remote set-url origin https://github.com/vixinxiviir/biject.git
```

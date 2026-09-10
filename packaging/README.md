# Packaging

How Bijection reaches people who are not going to build it from source.

| Channel | Platform | Installs | Where it lives |
| --- | --- | --- | --- |
| GitHub release installers | Windows, macOS, Linux | the desktop app | built by `.github/workflows/release.yml` on every `v*` tag |
| Homebrew tap | macOS | the CLI | `homebrew/biject.rb` here, published to `vixinxiviir/homebrew-tap` |
| Scoop bucket | Windows | the CLI | `scoop/biject.json` here, published to `vixinxiviir/scoop-bucket` |
| AUR | Arch | both | `aur/` — see `aur/RELEASING.md` |
| crates.io | anywhere with Rust | the CLI | `cargo install biject --locked` |

The installers are the desktop app only. Tauri's installers do not put `biject`
on `PATH`, which is why the CLI has its own channels.

## One-time setup

Create two **public** repositories on GitHub — they are how a stranger's
package manager finds the tool, so they cannot be private:

1. **`vixinxiviir/homebrew-tap`.** Put `homebrew/biject.rb` at
   `Formula/biject.rb`. The `homebrew-` prefix is what lets users type
   `vixinxiviir/tap` instead of the full name.
2. **`vixinxiviir/scoop-bucket`.** Put `scoop/biject.json` at
   `bucket/biject.json`.

Neither needs anything else in it. A one-line README saying what it is helps.

## Per release

After the release workflow finishes and the GitHub release has its assets:

1. Take the hashes from the release's `sha256sums-macos.txt` and
   `sha256sums-windows.txt`.
2. In `homebrew/biject.rb`: update `version`, the version in `url` (twice), and
   `sha256`. Copy it to the tap and push.
3. In `scoop/biject.json`: update `version`, the `url`, and `hash`. Copy it to
   the bucket and push.
4. Commit the updated templates here, so the next release starts from the right
   values.

Then check both actually install:

```bash
brew update && brew install vixinxiviir/tap/biject && biject --version
```

```powershell
scoop bucket add biject https://github.com/vixinxiviir/scoop-bucket
scoop install biject
biject --version
```

The Scoop manifest carries `checkver` and `autoupdate`, so
`scoop update` can find a new release on its own, but the bucket's copy only
changes when someone pushes to it. Automating steps 2 and 3 is a small workflow
job once doing them by hand gets tedious; it needs a token with write access to
those two repositories only.

## Signing

Nothing is signed. What a user sees, and what to put beside the download link:

- **Windows:** SmartScreen shows "Windows protected your PC". Click
  **More info**, then **Run anyway**. The warning fades as a file builds
  download reputation.
- **macOS:** the first launch of the app is blocked. On macOS 15 and later,
  open **System Settings → Privacy & Security**, scroll down, and click
  **Open Anyway** next to the message about Bijection. On earlier versions,
  Control-click the app and choose **Open**. The CLI installed through Homebrew
  is not affected.
- **Linux:** nothing. Linux does not gate unsigned software this way.

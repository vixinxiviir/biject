# Homebrew formula for the Bijection command-line tool, macOS only.
#
# Lives in a tap repository, not here: create `vixinxiviir/homebrew-tap` and put
# this file at `Formula/biject.rb`. Users then run
#
#   brew install vixinxiviir/tap/biject
#
# This copy is the template. Per release, update `version` and `sha256` -- the
# hash is in the release's sha256sums-macos.txt -- and push it to the tap.
#
# macOS only on purpose. The Linux CLI is built on Ubuntu and links the system's
# SQLite and OpenSSL, so a prebuilt Linux binary through Homebrew would break on
# any distribution whose library versions differ. Linux users have the AUR, the
# .deb, .rpm and .AppImage installers, and `cargo install biject --locked`.
#
# Installing through Homebrew also sidesteps Gatekeeper for the CLI: a download
# made by curl is not quarantined the way a browser download is.
class Biject < Formula
  desc "Compare the schemas and rows of two databases or CSV files"
  homepage "https://bijection.dev"
  version "0.9.0"
  url "https://github.com/vixinxiviir/biject/releases/download/v0.9.0/biject-0.9.0-macos-universal.tar.gz"
  sha256 "REPLACE_WITH_SHA256_FROM_sha256sums-macos.txt"
  license "GPL-3.0-only"

  depends_on :macos

  def install
    bin.install "biject"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/biject --version")
  end
end

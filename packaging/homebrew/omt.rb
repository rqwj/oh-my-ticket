# Homebrew formula SOURCE for OMT (plan U12, R20/KD1) — lives in this repo.
# A Release-checklist step (docs/runtime/distribution.md) pushes a copy to the
# org tap repository (rqwj/homebrew-omt); users then `brew tap rqwj/omt`.
#
# Consumes exactly the artifact contract of scripts/assemble-release-archive.sh
# (U11): one GitHub Release asset per triple, extracting to a single top-level
# folder holding omt-daemon + omt + README.md; version = [workspace.package]
# version in root Cargo.toml (KTD1), tag v<version>.
class Omt < Formula
  desc "Task ticket orchestration: omt-daemon + operator CLI for OMT homes"
  homepage "https://github.com/rqwj/oh-my-ticket"
  version "0.2.0"
  url "https://github.com/rqwj/oh-my-ticket/releases/download/v0.2.0/omt-aarch64-apple-darwin-v0.2.0.tar.gz"
  sha256 "334fe8847f50cbab36d6d19aa7be542c90297563a58ca610540f7ad004b7e2b1"

  livecheck do
    url :stable
    strategy :github_latest
  end

  depends_on :macos
  # R19 ships the aarch64-apple-darwin asset first; until an
  # omt-x86_64-apple-darwin-v<version>.tar.gz leg appears in Releases, refuse
  # Intel installs here instead of failing mid-download with a 404.
  depends_on arch: :arm64

  def install
    # Both binaries land in bin (NOT sbin): daemon discovery (KTD7) probes
    # PATH-style bin prefixes (~/.local/bin, /opt/homebrew/bin,
    # /usr/local/bin); sbin would hide omt-daemon from discoverOrSpawn.
    bin.install "omt"
    bin.install "omt-daemon"
    pkgshare.install "README.md"
  end

  test do
    assert_match "omt #{version}", shell_output("#{bin}/omt --version")
  end
end

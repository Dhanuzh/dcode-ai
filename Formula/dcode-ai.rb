# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.28"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.28/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "d036af6c87d7a87c63ebf943c4b62528ac8c9ca5558ad242b01bbc75c3d0b853"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.28/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "4c31eef0a7c9f88bb3f19f0347a53923e74851b889fff80a741d3b68ec15fb94"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.28/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "aaf4e6106f1883da6f3bdd2e3e68d99bc82ff77a206ff49bcc5cbaeceb620914"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.28/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "f1ce2e7efcf2c3bb9d31e6714240cde12fac7bd84d12e37bc1964bac84ca9e6d"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

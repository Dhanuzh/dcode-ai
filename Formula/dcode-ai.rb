# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.44"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.44/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "7fd5e3c601337b1ad9eb5205229691ad818605038571c7d1e831fb9237fafd24"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.44/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "3e4e6a9739f3088d0ce23596c4b23d523719d3fa463e0ce822a052f8366041c8"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.44/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "085679777ece3b2ef7f04a2cb100f0ef0a35eef7fdbd29830b746da94abd751d"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.44/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "beadd190d55fa677259e377f5bd2fd7756ccfe20fc11bc03a82b547e2b272339"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

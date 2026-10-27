# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.26"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.26/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "a1008c29727ffee46e7b442fda8b55699d922d596efbfc336aed629715df7be5"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.26/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "af4550e2a645b5f2fd6d7a3805e1c87eefc8163c3ccbd6073962c75ea0d99d12"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.26/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "733f0f88de517afb014c99b77c2522255cc718c0ee5c5f7d5ea35a311a8de946"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.26/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "fa0fae14a68f2fe63ab0502c7a508db83a70b3f88de93805551e173a5e791583"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

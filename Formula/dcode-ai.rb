# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.66"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.66/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "d349cedbbecbde660d9d5c8f00dd8a5f88f73640d0e080ffed4269ca182a2f4a"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.66/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "3a997fc43fa9f28592d75685f606f0de01ff25353df553e18071a62e176c815e"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.66/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "3e32c0e757d67c9c9b33837ee8f3604f61e5e684b16a3539b9a2dfb98adfee63"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.66/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "48a1df0bcf0e0a10284e304c6db7968e406dfd1c26df6333e8f5a2c8ba8e16d6"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

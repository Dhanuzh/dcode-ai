# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.72"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.72/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "bcd40d9a0a720a1b703309f3be4bf98b5b640e40c2dbea14ace02fd137723671"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.72/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "81e0b2a8e97be1e846661e7f791f263fdc1d9ca9ea24e7ec67d891f16f798e52"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.72/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "efda5def577c5d21ff935e4d728f100487ffc7b904344d6df2d54c4f5e8a1b36"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.72/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "f2d0ef91d17bab5064d811def47f2767ce40ecf2a00e522effbab0d0cfec4c6c"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.31"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.31/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "e7cb2c346b1c59122e7a16c4d46c19390057ce7ec6bd8b8701afa1939f6e06dc"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.31/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "a2d06fa09ff7549176bd9be996cdca7723ee8e5f43a075988bd73434a160b0ee"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.31/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "dae91df561e184e9557fe676f6cb639231d58478e6b21af2afc7484a0daed705"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.31/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "c04b16d255152813f0c044eebebde9fdbb6d140cf08ef13f933a427eab900aee"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

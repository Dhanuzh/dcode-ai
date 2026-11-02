# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.43"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.43/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "429a2eb47b19c2148d753506cffdf656b1e05c73209ac985298275d502ec02c0"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.43/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "2e6898dbd50b9b4eb3ed2d8f68508bd8f58dc5ae6f4df1178a57060c81d64fa4"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.43/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "06fe91569fed5d23ee618a60cdfdfcaea656b15a520ba4213f57c3ea45110259"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.43/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "a3ff2234cbaf42b1a90f236d347c58f8e6b3912102c73ce2bdd82604d189beec"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

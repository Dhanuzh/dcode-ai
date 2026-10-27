# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.25"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.25/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "28acbda17c4a9b020237995d18881bd5ac9a656d2136e484c5810a19c4d03b62"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.25/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "74eac3fda08fe0461022d1bb2ea8e17989a280b08fac7017d8db1db50eb5a67e"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.25/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "c72772dfb7d65d29f1c6d350f436e31cfcb522e197bb8ad494931f9d131b7b63"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.25/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "285bcc6f9b19c64c605ecbb3e3bee49b87f04884b7829f88ab806469d7cfe1f0"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

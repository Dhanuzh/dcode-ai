# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.67"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.67/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "3ab13c50ad64250756795fd2e1cc7b75200bf682af17639ca51b1268f3b14ff2"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.67/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "ef2ceca01024d2ec6bbd2d0a4a9a8b4ee9fbdddcec1cd3a5b3971a060feb8221"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.67/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "64cc4f4bc1d9027ed7d24d767e268a9997d17f5a0f84a9f9acb60e3a4b9d975c"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.67/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "07a0a0b202f7c5fbb03c9c02ed6ed4c20067c42ecc5fee56b071068284499394"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

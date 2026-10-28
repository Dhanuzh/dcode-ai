# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.27"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.27/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "edc5b027637e047879330cded008cb57205ecb14693f9666c3c67abf3905b104"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.27/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "5d1329ec06cf248ae4547fec6918669a3ad7389e56936ccfca22040c2a627aef"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.27/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "6a3c9d62918fc01054498989cec555d8880f44fc25bd5439d371b0b05aca431e"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.27/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "86ea7eb9731356549a174ee30bfdfed92c590c9306c38a3734d111145c0c23f3"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

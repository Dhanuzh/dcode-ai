# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.38"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.38/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "330da3be5ff409c6d4e93f497bb0fc4a263ed7747401acfdde3e42d09ba5713d"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.38/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "ae9f38129fc54824dae7d919272bb758b888a643dccae44808717f05ba096b5d"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.38/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "c09ecbe20c985ed42591577ad63f65964e0b69510a784a9d9b9c639b15b9992e"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.38/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "dec605dd055cad6673b47eaf481b7417a2d8fb5669d97a56762cb1c188754c57"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

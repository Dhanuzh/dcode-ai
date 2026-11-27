# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.96"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.96/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "318076df84f17af75adfb308ef4524ecb3d4b6749a9fa4aa669147ca62a9991f"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.96/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "31a286e5ed7eea5de5dac14d81e53e82fc3c2443c6563844eed0ec63d4c5fd93"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.96/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "b87ee4d6e42038b6207f892976894d1917be0faec30d569886b02592136f28d4"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.96/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "615d5cc2dfd320ff2a5e998f5f5d0def67861eb3ecfb27aa945abbb2a9523b70"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

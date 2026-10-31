# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.36"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.36/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "722ea736f4dd49e5525ae44faf561585742288ac413ba48a5a68c4b288e200ed"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.36/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "ba953f2174b1b1c4bbf48073bd3aceabc859cb132c70e4df33b3794fe7cf532b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.36/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "114be49efefc1a95d7bb07b9f48990b6b1ff181c38d1f1c48e5b1129da9b9893"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.36/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "55a24cd1d6bf5d4dd35e84dbd5c5a54cdc224ea9bc5c53a036487859f488f3aa"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

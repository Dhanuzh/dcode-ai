# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.70"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.70/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "e20bb7be3b7d95b695502ea9b9813e087e934d709d22bd56c10e766b73b956ef"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.70/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "c64943fa9e9dd2659a2e8ff28c97f59cc9fed84b70d7171e433282ce434428a0"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.70/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "ca66d16c5e212f1bf16166765274ca563760ac5141a063a3dfcb66f9010abd20"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.70/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "bece9ab19eb574b79a12f40c01f4eaff90d7af5ab6a73b1d0dccc3029d41eed1"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

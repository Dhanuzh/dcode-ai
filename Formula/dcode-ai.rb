# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.45"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.45/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "29a2000718e422f988ff2fbb9400f4b296732feb1a56516aa726eb91b2c9cb06"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.45/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "afa3602093e29284adb12afb1a6ce56af929238861cf32105d11890b9bdfdd02"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.45/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "d6c642fa60daf1c1784b8237fc2b66b2e1fc43d994e9f816b1c2dfadd6b517c4"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.45/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "454932bbafc947c72b0f5db174bfcddca8bc4ef1389a71edaf27e879441e10e8"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.56"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.56/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "253456f69272765478c8aafc8e0fc2cdf0c9e66f9249645fad62568b1cbed650"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.56/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "3a37ce42186f214b7fec83c7787c120fd3972531666e6378e216fb6c5095df6a"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.56/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "5c0e10988d8eca7845a21dcc023d802f9fc471273a4f46ef1094f3f43afc0789"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.56/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "852579a60f375563cdb997b1a86d60fcd707ffa993b1381616a4db7b46c3ac53"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

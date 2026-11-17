# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.76"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.76/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "a2ef38850e8098a3077355594d878213b3385e5c3893e5d8bb6f0967e0159ec6"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.76/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "d837e137857139d953882b141403191c947a2dba72a56bfdd7d02ad62a11ef53"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.76/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "deac368fc16640950e6c4bcafed1d2738934018873e9b052b66c168324e79024"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.76/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "e6bd00198f7a1fa407f98cc9fc894351d79109c67326bd6bda49147be8ea0d20"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

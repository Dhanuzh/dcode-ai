# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.71"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.71/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "47eee2ff1715d510ee056cfa017288180616802d767db8ac7c756f08590139c7"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.71/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "785d0bf7cb8031e64b34d43d469c4556801babb9bfdb5007ec537d07bbd5686a"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.71/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "e25d3fc511e1af880f9f21f13fd61900a70553d95d2d429b0b45cdbb6f9edd9a"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.71/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "962b0c80227af57b53fcd34897d3ded19bab4cccdb5dee6d76abf550193f8cf9"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

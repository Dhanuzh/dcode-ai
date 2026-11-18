# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.81"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.81/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "c179b55e4e4833d83174fc4f4ba995b7db1df76575df17c332114593be0feeb1"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.81/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "d4b297e7e6e71319f4cce04a59cb37b137629c72b39252d7d720b4de1b732c6c"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.81/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "779b1fe1eefc5a24d2333f0c61b4e7cb233d81812edb996d4449dc8127064ba4"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.81/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "18cf90c25a6961cbf6320b443dd3ef23da74aa8fc966ee32e450eb88f3cc332d"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.88"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.88/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "94312d460e65986de94053c7b21bf4fb63a95db6fcad42b127a6a8ac6376bffb"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.88/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "2c2a24957ccd47da4ec6bb92f08042b0a9b0e41445eb8a5cbeff99042460f04b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.88/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "38c7ee72cf0dfef8f3b13e16cce6c579e101b2761741c1752f4d0baf2985241f"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.88/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "d647b357ec9783a8b747698096ae352179fca063fff146fa276f399002cd66ac"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

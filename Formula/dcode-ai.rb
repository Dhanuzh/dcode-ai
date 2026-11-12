# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.61"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.61/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "76fc6e37bf4535904b88ce53ba59cfffea94527c9a7a17c57505e0d4db64f001"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.61/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "f508736596a8de476e3b98ff8454e945b2c19e6b8da71b7d07ac23d5e97b1f36"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.61/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "dfe676827f6a1d7618f4bc15068e5fda9364aa68e8a6ad6d6c156121688b12ff"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.61/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "c7ffd9c8164b2da62af56b488f4ec432430ba2231ca03128abde09eb963a097e"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

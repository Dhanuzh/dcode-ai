# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.94"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.94/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "3b52201867e11ffc96c6f4e4a0f3f75d5106b894f5b4ac7e5646982030a5eaae"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.94/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "dfd40fa15541a7818eaab26ce8b210726a30e3dc83aa76b595f72e1a7f275636"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.94/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "fe3c3342b2e18a216be4523df80f58148087b44b4c3433f66e478e10a2c9538a"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.94/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "ef9d20a7eb1cf878067e3581f3473ed8f61f7378c3ece791e4d0eb14a7c485bd"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

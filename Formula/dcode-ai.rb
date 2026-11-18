# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.77"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.77/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "aba7bbf018a03b7cbdc3fb528e4bef27c6cb7fd8190f7891ab0858dc3de6d226"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.77/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "5b5b9f4862ed9b419b0a3b1fb8b6bd4fc9b0d6a74758993f6f15b36d54743844"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.77/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "efd9e3d4d28ef81cce8027338625729d7c87f63ab68916671e9d340ed25801ec"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.77/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "51130ca67d99425b4be475ad5f7d868b1662bc4d9d9c0de06855381f8d97c4aa"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

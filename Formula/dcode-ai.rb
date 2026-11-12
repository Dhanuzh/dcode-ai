# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.63"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.63/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "d1e1b3a92943810db41631ecee7fd3c9e590909e69d5edfbf76afd870b50753b"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.63/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "4ca366245aaa968565e2ffc5125418fd3748657fa831b3a28a8f3578c08cb13a"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.63/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "400b5cda41466a1a032c85ae4f0ae8458c4ef7fc41225b838b0800f2a90f8928"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.63/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "f5ad8137af86e7577ab95a53eb7c8bfc8e1b75b9504e959d3c6433f8b40475f8"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

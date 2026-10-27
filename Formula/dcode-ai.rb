# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.22"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.22/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "8fd9459a403e99d00e0f7eb7e8319e3a80a62c09058b4bef511b816f1aa492ae"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.22/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "7d47430ee28fee5529d152d22b2c633233082266f17cb7c7f9c8aa418f9cdf7d"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.22/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "dbabd6b8c53d2b955dccc26801c92c69a35559e9abdd6e73a66072b5b56b8a6c"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.22/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b95196ebbd4e72643892b576c07402cfdf3e453b15ad860cc923e283e464c6ef"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

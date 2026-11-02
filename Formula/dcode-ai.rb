# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.49"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.49/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "d5e3846e2a633145b197eb60b36319f95af37a4f03045c98b0fecb1ced899885"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.49/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "97ae0d3da347bfe31c6a4fef593d7cb3fedc16d1cae62e3aa74d96b74b2a0fc1"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.49/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "6866d58ada5a5033c456f03e0c381ab7408ac51ac59d48e8e2ed9f8885c33af9"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.49/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b35e953daa5e89a326fccbf228f51ab7389ab4c2a921e75da9221c59081399d9"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.50"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.50/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "0b9ac0e6e1e0c7f74f977efd79266a48b7886faf48a4820bd947024d53e50c6f"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.50/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "8a88af99c9b1668ae8cd5bc45d267a9ea643fd6433d210c1e558b215f9daf003"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.50/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "f52d2ff4c5c26397c6a11b87f110746866fa81f2447ba0b5992d14d560437085"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.50/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "cbf6ce5cc725d87ae920ad1b30ae5eaad66bd3fb663ccd75098ebfea31212555"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

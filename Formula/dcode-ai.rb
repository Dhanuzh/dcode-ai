# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.24"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.24/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "13a89f7c050f8f0d6eb6b34444406260931fdb6d8295b24fba0ed955a979ef69"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.24/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "35ff21afcc03eb969731b334417d3cd031a5b8fd6447612f4223c00e3a981107"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.24/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "7d3245d3953fa69487a58cf8c636b4a7b227c24a93e0dc19643d1f708a6622a2"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.24/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "079286540199a451dd22ec723f40e136ac43d0994123f7538418b8810d1a2e6b"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

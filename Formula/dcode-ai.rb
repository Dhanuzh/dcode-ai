# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.79"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.79/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "0ef238bdbfd7a0fafdea81600cd95d77ef8611677eb4eec06f7468b364159014"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.79/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "bc2ec22b11a0f99f2f75216d51f5248b6407824f6daccaa770607d78d33e5f6f"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.79/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "d886d11ece1dd4fb04175989564061b46ed67620f49a4e92ea54e0bc1927120c"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.79/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "d2599f683c5e2e1154c4f1272026e3a09450c12c234a2987488e3c60e20db58c"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

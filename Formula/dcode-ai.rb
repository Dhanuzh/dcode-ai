# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.60"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.60/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "9b8e08786039eae9b047b33d0a0f51374e483fd89c1ee49ed40d916ea8afc34e"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.60/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "5cb3e641a168a531126ef2693eac58f0042857bf371db4ecd442fb1bd1b001fd"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.60/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "e45da0dfa87a0a0b190c95c5de8396d58338881b8c59f58f0d865d8ca7bbfdc6"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.60/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "84051f7e14bfcd59bf9998fd6750902f8cb7bb1d2ff6dc2632e62f044ed6cc11"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

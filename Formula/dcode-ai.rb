# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.95"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.95/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "c3f096b41effddc7224590134e10e48e35b21ea40824921100fd2bd8ca825101"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.95/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "8e8bfc7b33e7c7fcb0d2a1ba587bf81fbf8fc3a6e705656b8c13e95b0dcdc20f"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.95/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "85fa128fa6ad4ea775f4bcf83d06bf9273bd3164388fba2ee107d79fc1d37e19"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.95/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b2862f7553eddc0e707e7996c1b3bb14ba123a1051daca03f5f8c73676838a6d"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

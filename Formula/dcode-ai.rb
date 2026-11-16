# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.69"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.69/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "bc4a5801e76d6a30c77d9a1415b046dcc10293eb3c09650637de5adf3b52cd66"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.69/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "248abdda9192e01ac42b33b35d5861d42d9138eab51552ccde070f3756878571"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.69/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "a60333301469723ef2ec281ac546cc846f30df1a79c41129955050007d0497dd"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.69/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "156d682da2a2a25223a1eb23da8026c80788d6061e45b77f66d2e41ec5562fc6"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

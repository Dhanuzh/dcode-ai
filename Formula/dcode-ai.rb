# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.47"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.47/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "960c7d2a78588f8bd1c2410b38681d0538cb7dcc5520c17cc620b8331466afe0"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.47/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "7edd548403faa85edccb9b749963a0ab4218972dd9fd1070e854fa620a90e598"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.47/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "78954f4a39b99a775d83e59f9844a66c420734f46fdd965a38ac64af8ca8a7ac"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.47/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "26c841bc6bbf3d97b55b9a2d74423fc234242e0701dd8735edd19e8102800c1d"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

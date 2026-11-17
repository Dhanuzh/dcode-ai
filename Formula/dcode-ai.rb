# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.73"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.73/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "1d627a25c28e1778ce09cc43ba6d8289a2b8e00a1e3adeec480736e71dfdaf79"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.73/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "fb28d3b3b4aa288cca932da7dc178f7982041db3e3eeeb35a43eb8085475e28a"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.73/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "8cb3a5f1c30e1d6902142542345b2ed1ade5da79b7900588f3449aa04b9b27db"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.73/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "684d05233ec4fd70dc0fbd6d6e4a95a96284e6d27b8fe88f8b9b9458b090e80c"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

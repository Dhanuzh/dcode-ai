# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.58"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.58/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "d6e8199a43b484dcabf4e154de561b23b25a4cfa67c870023f84d7fecfa309e7"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.58/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "a9fba52a8c544ea0f82140989851485920e926da70e3c1cd1951750bfa939f97"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.58/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "c1a60ca26dcfbe5d468600cf29d65420884c3271c18f3965fbcc60ba4393ec2b"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.58/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "4f618b75c2d1073dc94e9ca4d13aaf3a8ea6176fbf8737442732eddd03aea63b"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

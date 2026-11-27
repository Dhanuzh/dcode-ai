# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.97"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.97/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "19d9dd431790d04cfac7e6e56899ed68a4e4e5eec1c366a5a6713a3d9d35e86e"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.97/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "282a8190840d2bb116417cb3f9ad5b1d4164dd227f8a85012e9533883d939916"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.97/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "6da19261d580cf21a62e772b1322de70b8d5bc37ffb4a69d4e4a765198ca5e60"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.97/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "85ff3b3489464723a59a96447a61441a7aaf9427aad8642e9bd716d814da9f80"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

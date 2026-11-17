# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.74"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.74/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "d6c1b89ec43374d7d462b5a17b9da12d6bf54ac6ae4842a4023ab1f8b6a98337"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.74/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "5b351f10c5583fb4dd66dc59699f215ae76a88d4cd910432a21c1caa03de8d9f"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.74/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "93a0a90030f619e6beebc4497815631a15dc625ccf588356416132d884b58fb5"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.74/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "347790d06983aa629c075951b1eb669f16abba61d1bd7a91c7030a33c86e59d8"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

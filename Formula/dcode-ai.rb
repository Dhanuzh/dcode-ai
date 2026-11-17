# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.75"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.75/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "f239bd0b10358968ff08e9ba5ebba974327ce6127a09a834aa2d4d4b03af500a"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.75/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "daaa51138b5310ace57ec3d32d15c45135a0e42132dc10afeef1fe1b839b8d60"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.75/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "7b792b19960df8fd065811ddd28d2b7e76a27071f652d39c8cd65d79aae4f5f2"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.75/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "e9f8335b1486085592f367d8043d281a55fd4710a91e8108511f991a27c3b289"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

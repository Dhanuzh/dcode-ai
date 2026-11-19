# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.84"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.84/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "3367ebf94d0967ed0b30fd0e17707592aeeb0ebcb02f350882e2d0cf6a26b934"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.84/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "07a9acd18848b11e4826284a5ac13b729043eedbf64e9f081d4de67cc8552a5c"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.84/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "8d4fcb69e7b5b69c7872584e7ddbde9e7067f0e49f86e6df13d5b6a0a82175f8"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.84/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "fdec6e8bb7beae0775b853ef0dedfd0c4da871323e4deb002e8c87f0edf46876"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

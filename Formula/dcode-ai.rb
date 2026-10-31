# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.30"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.30/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "29aba1257413dd16af74d78df4fd8b716c87052853e36f90b36d68151a7de8ac"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.30/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "d2dd690a3f28bb8ed7acd76a975bbd9891dab0c8ec44288f1ccdf34ecb345697"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.30/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "8300117bceaaccec74f31454a411cf61ac91caa58f4cd175f5adbdb686ed029a"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.30/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "7e8db28875d913f441db01ae63c52014e39530f890082948242eb17d1a599d48"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

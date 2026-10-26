# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.21"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.21/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "90a232ae286a03c6e4d7d33f443614ce1dab8b4d9d17c3283b0010f9d330191c"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.21/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "2b19114a8c1b5b5222e9619d78ac4474c5f489d0a5320ffbfd8686017cacafa8"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.21/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "f8ba5491d120a22189489825d6163e406ecb24458ed770115fb96de4b308d2a8"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.21/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "be7ce1bb2329fe4eb7edebb9b36c51e2fb38b9e9c314d3f72b15707f54350e76"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

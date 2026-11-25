# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.93"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.93/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "f97577749537469cf153be35cbed8d2d4abd23b0f7cfd8c980aea9c20dfe71a9"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.93/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "088a201ad8c05860962de8402e55325d5f29472192a157fa2771af2183bcb26c"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.93/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "b6371149228575b93c5b75986d922283bf62d6d6fe2553d4ef77432e6bc79bcd"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.93/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "050dc8957ce9ec5c47f23c40dfced05c7cdcf542cf12078e519ed835195e6adc"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

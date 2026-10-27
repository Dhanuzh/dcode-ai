# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.23"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.23/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "b7368b15a8deab0b9e41bee993c6cc4d8fe09746457d56034549bd7a6572185c"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.23/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "bb1e47f4d8d74920c37943490e1c7a10880bf5e60588c1973657e47f4aa068d4"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.23/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "2cccaeb18b880769a581254b756fb9dc48d1a7b9eda162dc0cf8deb8c0a038cf"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.23/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "4e40c0fcd8ca962fb0515c9f89d3248e155e2aa8f2f72480206736fec271ceac"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

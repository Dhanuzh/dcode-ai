# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.37"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.37/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "86a50d0fa6403534ea52ec67675d403769dc22ce2184afa8c8f9a50e1114dfe2"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.37/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "0295a43366669246b14aed463b5447eb33a626ae136d8a588712fc9a0be464df"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.37/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "3dfea028921a476ee08ae4dddc0aa7298db3705d91b95a709ecc3ff5b302be44"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.37/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "383af1f7c2e191286bd68cfed94dbeb6c079b2f5f74584d50a9652ae59fa5075"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

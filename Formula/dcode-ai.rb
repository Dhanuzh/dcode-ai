# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.92"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.92/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "2bd85c23ae016a33682c36ab6d580034245dae7553d4aa45445bbabf2104e183"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.92/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "51bbbacf8451ef29ac909272fee77f3246df654ab7f81690d01b8e4e1ece1402"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.92/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "7036fe979bda4451b639ee3389b7998302e19a24adb46659868600bb98460562"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.92/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "67909d857c3455c9cd9d0371787446b8aa5d7dd1d618484176acbb06365084d9"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

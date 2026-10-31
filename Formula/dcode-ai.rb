# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.32"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.32/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "b0a02cd31a7e0af8bb01e5d985993d3b3a870ad1ffbf2d5ebbe13678802fabe2"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.32/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "400411da093b4a313f733ee92081d1f6a0dca45281ee7a4e5da061c6f2011277"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.32/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "9bc4a43fffb615656530abe34a1616396ccd1c714154b4aab2f6d443f00a6560"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.32/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "75d3fe09b42d38297a505dd3fcc447457c5f7220cc2f9a6f6f098ed9df1481f7"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

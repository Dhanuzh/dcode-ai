# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.59"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.59/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "49c07b64453a950078b28fbfbdcf024ebf98c5b8636f9ad15c3ec66dc4de1615"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.59/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "37e8be83f3943dfa94751e6c467f05912ef9657dd8ac836b251f6d90dbccd7f6"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.59/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "eba11bbf8780d214446376f4435710befd1cd8a0257429edb206fc7ebe218fa1"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.59/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "9165f3ad4dce0263fd27742ab13c880986c21ae360f4e947a056d8d1c844d8e5"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

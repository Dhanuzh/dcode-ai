# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.80"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.80/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "6fb207f9974e1b66862b6cb92a1fd006b62b74f214095758a35dfca32fc8cf43"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.80/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "ec3443d8937737a0cfab21f42a7ea6c695690f92245a341a00a7cb861cadb924"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.80/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "4bdf1cb4d0c937fff264d461e858f2e9887ce0f4abf7505dcda64b2cc5063a7e"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.80/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "99f45f1b0e4c3d51b2142c2d24ff804f6a0687e3b6f2c07b4535491f74029f2c"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

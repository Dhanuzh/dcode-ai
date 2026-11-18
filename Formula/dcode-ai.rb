# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.78"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.78/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "9dcb021fd5d449450d96ce5a2ba20428a787ed50bb4e9208822db53ecd3d04c1"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.78/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "c9057ad8607f7beaea302f7870e079dedd81f6634eeaee0ffa5a75f74a3081c2"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.78/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "1b165b081d4c98c4e1bedbaf23960f138245935b39fb6418bdaf62822a6b5ae2"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.78/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "884eb80cbb761e8cb62cdf0b590d5c3ac16bd98f0bd1017e5de2336420b7d29f"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

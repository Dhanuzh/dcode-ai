# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.83"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.83/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "8a4fc1ff1daaef7744fc005b4685d4a4a421b06fbebf8922138a099b2231ee70"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.83/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "0b42bab2c827ef7f9e7dfd90bb3d0474a82d22a429fecbd69cb6b2e2c6b3014b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.83/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "e1120c04275e805bdc6527f8cce50ddd2bc42171d659531cbbd8991787548ada"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.83/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "1cad80696d5292034e5fcfd672a3c7fcea5e87883fe806bd53edbe894b8f892c"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

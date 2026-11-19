# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.87"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.87/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "f944e832a915dd006e7a16a4951b8cfc271605740e2ae52dedd22703dce2fe3b"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.87/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "4ed38357c43c9086a828757c3153c4440669a32da8a69f521de5afd29f3440a1"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.87/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "b4060cd3ede4d02b5bfff4224e50b4734dc81ddd90df46885300222d66854d9c"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.87/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "57e428ba5efcd5e0d3ddd3c02a8217da1fca56681b17f6a84ad96e8c841991f1"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

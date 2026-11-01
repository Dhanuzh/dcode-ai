# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.39"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.39/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "688fde1eb190569f057248fb3cc445c1106ec7b738ce2c9260d7cec1be475415"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.39/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "453fe243805432ab317154c2a53661411a11b4345bbb94ae4cc49dcce4fdc67b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.39/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "b3409b92abce94a80ba2b65941f2a9196e37b782d56049ec5187562f8f3df035"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.39/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "6836e7ccc36d72a2536ba3eff69fc3e5502fa072be0a91a0657d6ed6781a4bc9"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

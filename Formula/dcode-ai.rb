# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.90"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.90/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "8b3b242ccd7571bef3be59488c9e9fecc0db602488d478a4a27abae3509c5401"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.90/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "aeff5a331061314d8f5762287ed8168319477b2ebc71b53cfc3ccf506222e72d"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.90/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "5bab39d601d0a9ce3d1b7998898ee799f30c75fb5f78f046d304c8453a8e6dc7"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.90/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "3327c083d45484536a24a594511f1e01f65614f10e90386825e40b02fb3d49bd"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

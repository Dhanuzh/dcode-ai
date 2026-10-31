# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.33"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.33/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "499f8e4fdec7284f8aba0efef1087b3d14431a16827a543294ab9712c55f6a68"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.33/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "16a0d1e7c1f016163312e828c2299cd3010bc522430c7ff7755fe4a03cbf40eb"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.33/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "1ed75b47c7091b6d68a8595e9f1a3b05562ebf404778bf5542d1b90d6399ee1c"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.33/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "526f756394893a2a9ae808b14cb7b954f3ae08e2f72fc616fa2a2d9cd1d9c06b"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

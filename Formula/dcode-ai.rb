# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.62"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.62/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "00d62590f8b229cfb531d46de1c9e83f8f8d79029d6b1035f7fb8beffc4b1e0c"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.62/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "cb7e254bab2408b42af2bc4901292e7e49b18b86f9d54e786f6bd581c8e4f838"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.62/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "d8e2b7341fc53724d36925ab1a2e5ca1b6a2d996c73f61c7520dea350c8e4444"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.62/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "f59b48cdbdd04670b98cea1343d399deffb3bbdb9c8db52019b0dca82ce7bdca"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

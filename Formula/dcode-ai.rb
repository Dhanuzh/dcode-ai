# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.52"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.52/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "00759e4b9943eda5194338d466d0d20840ae0dce63117c797e8cacde9420ea90"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.52/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "f015c35c2c6b97d8eb28b1f6d2cfe805f5ec231e6464851cb67dd72a12604551"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.52/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "4b975479cb5b9930f4e8b6851f68f7fa9667b01b5693d8d3f4a6d7d079bf18de"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.52/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "1deef468ead886925b5493b5e15da27c76dbf48f22d7d56bf313ae58e7519cfd"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

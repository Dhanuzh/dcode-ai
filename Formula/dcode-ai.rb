# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.89"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.89/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "188b6736ac9e75a3d8bdffc2cf8d9f11f50226dfa875731b119e96c6b73baa17"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.89/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "d0aa0c11c310ddace3f3512c5c3ced19863894f9e8b62fc2243b0504d8e9d8da"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.89/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0e8c713d689e32a5a4d246d6808eaf3ea370b9a7e37bc805c5192e0d8d823dfa"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.89/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "48154d15bed6cac3892bb5bafcc92a4394c6f2e0d301781d00f3bfc88ebe3bd6"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

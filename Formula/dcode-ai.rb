# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.34"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.34/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "b74cee16a926507548df343d06ba5466a866810186a3b25b96917719d149824d"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.34/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "b47f308b7b392b2396c103c34aed1aafec15e95949dee3071e02ff8e3f88c07a"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.34/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "6f7d619598c4441a30cc82220b9dbb125b2be90b055a02881dc3f8fd21712791"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.34/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "a12d2e47270ecaf3f3aef4323fff6b041ac2c3412333d052c637cb2e960d2bb7"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

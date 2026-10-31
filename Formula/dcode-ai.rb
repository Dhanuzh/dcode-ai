# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.35"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.35/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "bcfdbbe38fc9825977230632eaa8fc5bb95beee3c837fa059771d261a77ca3bc"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.35/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "258deea8f0370b317541ccd955610ece3e065b831ab2198706fb64568b604da4"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.35/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "58f803b14d7ef90309857944057972e40cd70a54781c5d01586623d48727896d"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.35/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "e6d1799ce93e316b79d9b40105d2e5687c6bb8f3d64f0ac0db8d596f2808e458"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

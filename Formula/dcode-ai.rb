# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.64"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.64/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "16eea29627fb05cb14aa62956b9dedc7879e297ce3eb6d905c2ae241a796459c"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.64/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "1031e865174278d79e81a6fbab8b554528473a923ed053590fc4507ab11572aa"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.64/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "a7f119288f0ed3a665f8345970fc9afbbc15a01a353b8740cd4f36d0db7edef9"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.64/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5423fa59a9de31db6a0d83b68b6cb29c779eab2589534a5acc189d57ef0b6a15"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

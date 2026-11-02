# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.46"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.46/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "7f871a4dcf2b1fda196f7b30b3ca6d30e2169b5868edc046c9f2519b4a219268"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.46/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "ae7a8932e426239d7d4dbca91891561723d210648998b25ed5b33521c3d83786"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.46/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "d7151320dc10190db3c4af88062ce3b7c885a8aaf55e6dba726450b1ac76c09c"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.46/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "a161a8997cca2c2fe134bb7668dc2dedc19fd4e46dcac854c73d8d368829ec68"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

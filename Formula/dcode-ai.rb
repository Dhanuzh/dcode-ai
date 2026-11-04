# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.53"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.53/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "33fac569fe26335e9b4998ff5add74275b39ca3a20f92a9a48c00273898aa2a6"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.53/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "71a3c96a8fd4ddaec53445d1c29aa3be5ec84273c946ce0564132de5a8e26b3a"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.53/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "c6a93167d134a88cc9a337709a98700600348585800815d7654f7fd7a6b40d15"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.53/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "dbe0d7c016ab13d35c1d7f57ae8998bff021f0a81966b4f8c6028c2d6c46a550"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

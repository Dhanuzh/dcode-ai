# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.85"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.85/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "65572aa04186fc7134c505695435e57b0fd5f4c489e5b7ca3f48d64b2f524249"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.85/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "1ae3670df084e8d8edfd4bceba4e4f617be911a74148c3e9ed7e0e17a114bec3"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.85/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "f81bbdcdf625529f20b15dae9bb090990ca86c548d00b1eabc08f85f07b917ba"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.85/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "7505fbf72659c88d38ab321fcaef5755e03040cf6899eedffa936c0a529dacd2"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

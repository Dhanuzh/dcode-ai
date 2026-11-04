# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.54"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.54/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "c7f97ebb734a89a9ae202af4af0aa2cd23041d09621c6a7d17dad07580c04d90"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.54/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "c0a817c109bec2c3db742554c958ba498d52bef97862af0087a95eb18692bc61"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.54/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "76d55a0220c7e2abd01c1359a87dbf1f3c532d7d7ca28e24721a2b6de98055d8"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.54/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "ecbd0a249b2d002342c65fcef5ed6d83a07759cb82aa498417ad0011a19720d0"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

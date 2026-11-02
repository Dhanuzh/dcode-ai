# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.42"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.42/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "20533164087d8e7650e47ade964140214166dfb8b619f354c80d246588673c1d"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.42/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "758ada91543adac0fe95b3f3e7eb494569faca51c9261ee4a98c975f837036c5"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.42/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "f37d1d8ef0adcfdc40afefeb60c2a915365139ee93e7b85eec5e9997c7cc29d7"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.42/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "aba8056383a3bff1a7f580792b6fe7bc75004137ab4a4a0c0a68d51d56f48e99"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

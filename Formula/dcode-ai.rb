# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.48"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.48/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "dd4134dd401f74523e1f8493a5df558ac28870a9e9c2bab47fff61513a2f8ea5"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.48/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "bae43517adc535011960b4182316b8e79769e0107650be368da919e6cc91f52d"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.48/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "7dbe1b9c6468a845d38b852e852089e79bb4f735959ad208259acbf55e1ce766"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.48/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "77ebbb9a46d805589c3c217b7a72aecdb5eece03e9c0e1b5bc356b4acd9a8d8d"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

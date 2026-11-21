# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.91"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.91/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "4527b01d15051f377f772dc1937880f839994d7435b406c0116cceb13d61ac2f"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.91/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "a21c5a72766305b9402817f449b6cb7f55f406ad0e43d4c8014a074e46064a16"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.91/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "6befce5d831439ccfa40fc4462a43427f97656a639a0df1bc911f4093b687b3b"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.91/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "c796bd69bf24374ac7e9167d3a16503d87b6cae9e0157cf6532c5f39c8b28501"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

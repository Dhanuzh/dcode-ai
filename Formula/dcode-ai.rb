# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.57"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.57/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "a73b803190b7834519972192471b6eea79ebc7fc877e9a843bfcb9443f26198e"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.57/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "d0378db3787aa884ed06ea862d7ef427c37e864b927b6aa0cb3cfaa0f895962b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.57/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "c6c9ba26b40ec3c71027067fafb2624bed7dfde068382ae1552aaac07deb79c1"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.57/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "c9895e5f662d8dec05144d4531c49e618bc309142ea154aa35e216e635ef29e9"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

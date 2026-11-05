# Homebrew formula for dcode-ai. The release workflow fills in the version
# and checksums, attaches the result (dcode-ai.rb) to each release and commits
# it to Formula/dcode-ai.rb, so this repository is its own tap:
#   brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
#   brew install dcode-ai
class DcodeAi < Formula
  desc "Rust terminal coding agent for Claude and GitHub Copilot"
  homepage "https://github.com/Dhanuzh/dcode-ai"
  version "1.0.55"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.55/dcode-ai-aarch64-apple-darwin.tar.gz"
      sha256 "6182fd47eef66206e8a7e323e9a428132706236913d3ed927cf4c531529abb9f"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.55/dcode-ai-x86_64-apple-darwin.tar.gz"
      sha256 "74645b38f49eede7c587f8896b86ee806a2f98b2756a888e8d4812e2fd96fcdf"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.55/dcode-ai-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "96afc79ce3b738ee61c0a35261dde2b585fa3e4f91fd9edfa3ae19a9ea83ed16"
    end
    on_intel do
      url "https://github.com/Dhanuzh/dcode-ai/releases/download/v1.0.55/dcode-ai-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "60e9e71ef6ed1b4a3613ef2c510b6d78684582b349d7ed8cc03f693b7023e6a7"
    end
  end

  def install
    bin.install "dcode-ai"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dcode-ai --version")
  end
end

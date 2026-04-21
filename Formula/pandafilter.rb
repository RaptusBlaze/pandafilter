class Pandafilter < Formula
  desc "LLM token optimizer for Claude Code — 60-90% token savings on dev operations"
  homepage "https://github.com/AssafWoo/PandaFilter"
  license "MIT"
  version "1.2.1"

  depends_on "jq"

  # Prebuilt binaries — no Rust/LLVM build dependencies, installs in seconds.
  # Each tarball contains the panda binary + libonnxruntime dylib bundled together.
  on_arm do
    url "https://github.com/AssafWoo/PandaFilter/releases/download/v1.2.1/panda-macos-arm64.tar.gz"
    sha256 "09519f78c686cd8597cb3f3b5b97a5fa1141d97d8106d632f05a68c72427bd91"
  end

  on_intel do
    url "https://github.com/AssafWoo/PandaFilter/releases/download/v1.2.1/panda-macos-x86_64.tar.gz"
    sha256 "6576337fec6a6efa67647da24826d8eb1872cfdb790843fedaf5e9fcdeebf9a1"
  end

  def install
    bin.install "panda"
    # Install the bundled ORT dylib and fix rpath so the binary finds it
    dylib = Dir["libonnxruntime*.dylib"].first
    if dylib
      lib.install dylib
      system "install_name_tool", "-add_rpath", lib.to_s, "#{bin}/panda"
    end

  end

  def post_install
    # Register hooks (fast, no network). BERT model downloads lazily on first use.
    # quiet_system won't fail the install, but we check the result to guide the user.
    claude_ok = quiet_system bin/"panda", "init", "--skip-model"
    cursor_ok = quiet_system bin/"panda", "init", "--agent", "cursor", "--skip-model"

    if claude_ok || cursor_ok
      ohai "Hooks installed. Run `panda doctor` to verify."
    else
      opoo "Hook setup could not complete automatically."
      puts "  Run manually after install:"
      puts "    panda init"
      puts "    panda doctor"
    end
  end

  def caveats
    <<~EOS
      Verify your installation:
        panda doctor

      If doctor reports issues, re-run setup:
        panda init                      # Claude Code
        panda init --agent cursor       # Cursor

      Then restart your coding agent for hooks to take effect.
    EOS
  end

  test do
    assert_match "filter", shell_output("#{bin}/panda --help")
    assert_match(/\S/, pipe_output("#{bin}/panda filter", "hello world\n"))
  end
end

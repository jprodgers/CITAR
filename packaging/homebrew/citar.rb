# Homebrew formula for CITAR. Belongs in the tap repository `jprodgers/homebrew-citar`, as
# `Formula/citar.rb`; see ../README.md for why a tap rather than homebrew-core.
#
#   brew install jprodgers/citar/citar
#   citar setup
#
# The version and sha256 below refer to the source distribution on PyPI and must be updated for
# each release. `scripts/release_checksums.py` prints the new values.
class Citar < Formula
  include Language::Python::Virtualenv

  desc "Civilization V-style 4X game for benchmarking language models"
  homepage "https://github.com/jprodgers/CITAR"
  url "https://files.pythonhosted.org/packages/source/c/citar/citar-0.1.5.tar.gz"
  sha256 "7d81063c228e3987e3e8cdda77b984d1c57af0b91027402e73ab4ce8b0311b62"
  license "MPL-2.0"

  # 3.12 rather than the newest: Homebrew's `python@3.13` and later occasionally lack a wheel for
  # one of the compiled dependencies on one platform or another, and a formula that builds
  # argon2-cffi from source on a user's machine is a formula that fails on a machine with no
  # compiler.
  depends_on "python@3.12"

  def install
    # Installs into a private virtualenv under libexec and links only the executables, so CITAR's
    # dependencies cannot collide with anything else the person has installed.
    venv = virtualenv_create(libexec, "python3.12")
    venv.pip_install_and_link buildpath
  end

  def caveats
    <<~EOS
      Run the setup wizard to find a model and configure CITAR:

        citar setup

      It looks for LM Studio, Ollama and other OpenAI-compatible servers already running on this
      machine, and tells you what to install if it finds none.

      Your games, settings and results are kept in:

        ~/Library/Application Support/CITAR      (macOS)
        ~/.local/share/citar                     (Linux)

      They are not removed when you uninstall.
    EOS
  end

  test do
    # `--version` proves the entry point works; `where` proves the package data (the ruleset and the
    # web client) was installed alongside it, which is the failure a packaging mistake produces.
    assert_match "CITAR #{version}", shell_output("#{bin}/citar --version")
    assert_match "saves", shell_output("#{bin}/citar where")

    # Exit status 1 means checks failed; 0 means everything a fresh install can verify is in place.
    # The model-provider checks warn rather than fail on a machine with no model, which is what a
    # build machine is.
    system bin/"citar", "doctor", "--quiet"
  end
end

class Drukal < Formula
  desc "Checks agent-made changes and reviews Dependabot pull requests"
  homepage "https://github.com/keys-i/drukal"
  url "https://github.com/keys-i/drukal.git",
      tag: "v0.7.3"
  version "0.7.3"
  license "MIT"

  depends_on "rust" => :build
  deny_network_access!

  def fetch
    system "cargo", "fetch", "--locked", "--target", "host-tuple"
  end

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    ENV["DRUKAL_RUNS_DIR"] = (testpath/"runs").to_s
    assert_match '"runs": []', shell_output("#{bin}/drukal --output json runs")
    assert_match "Usage: drukal dependasolve", shell_output("#{bin}/drukal dependasolve --help")
  end
end

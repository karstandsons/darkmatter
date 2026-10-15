# Homebrew cask for the karstandsons/homebrew-tap repo. The release workflow
# fills in the version and checksum and pushes it there.
#   brew install --cask karstandsons/tap/darkmatter
cask "darkmatter" do
  version "0.1.0"
  sha256 :no_check

  url "https://github.com/karstandsons/darkmatter/releases/download/v#{version}/darkmatter-#{version}-macos-universal.zip"
  name "Darkmatter"
  desc "Omarchy-themed Mattermost client"
  homepage "https://github.com/karstandsons/darkmatter"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on macos: ">= :big_sur"

  app "Darkmatter.app"
  binary "#{appdir}/Darkmatter.app/Contents/MacOS/darkmatter"

  # Not notarized yet: clear the download quarantine so it opens without
  # Gatekeeper refusing it.
  postflight do
    system_command "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "#{appdir}/Darkmatter.app"]
  end

  zap trash: [
    "~/Library/Application Support/darkmatter",
    "~/Library/Caches/darkmatter",
  ]
end

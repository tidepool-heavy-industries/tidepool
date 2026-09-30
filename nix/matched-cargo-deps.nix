{
  rustPlatform,
  fetchgit,
  harnessSource,
  lockFile,
}:
(rustPlatform.importCargoLock.override {
  fetchgit =
    args:
    if args.url == "https://github.com/tidepool-heavy-industries/exomonad-harness.git" then
      if args.rev == harnessSource.rev && args.sha256 == harnessSource.narHash then
        harnessSource
      else
        throw "Cargo harness revision differs from the matched browser source"
    else
      fetchgit args;
})
  {
    inherit lockFile;
    outputHashes."harness-0.1.0" = harnessSource.narHash;
  }

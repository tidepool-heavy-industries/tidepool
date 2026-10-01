"""Guard the explicit Buck compiler profile contract without starting Buck."""

from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]


class BuckProfileConfiguration(unittest.TestCase):
    def test_checked_in_default_and_shared_profile_validation(self):
        config = (ROOT / ".buckconfig").read_text()
        profiles = (ROOT / "build/native_profile.bzl").read_text()

        self.assertIn("[tidepool]\nprofile = fast-dev", config)
        self.assertIn('_PROFILES = ["fast-dev", "debug", "production"]', profiles)
        self.assertIn('read_root_config("tidepool", "profile", "fast-dev")', profiles)
        self.assertIn('fail("unknown Tidepool native profile', profiles)

    def test_rust_and_ghc_flags_follow_the_shared_profile(self):
        profiles = (ROOT / "build/native_profile.bzl").read_text()
        rust = (ROOT / "build/rust/defs.bzl").read_text()
        haskell = (ROOT / "build/haskell/defs.bzl").read_text()

        self.assertIn('if profile == "production":\n        return "3"', profiles)
        self.assertIn('if profile == "debug":\n        return "0"', profiles)
        self.assertIn('return "3" if target_name in hot_crates else "0"', profiles)
        self.assertIn('return ["-O2"] if selected_native_profile() == "production" else []', profiles)
        self.assertIn('rust_optimization_level(name, _HOT_CRATES)', rust)
        self.assertIn('haskell_optimization_flags() + _package_flags(packages)', haskell)


if __name__ == "__main__":
    unittest.main()

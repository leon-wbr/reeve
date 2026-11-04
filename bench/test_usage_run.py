"""Tests for usage/run.py. Run: python3 -m unittest discover -s bench -p 'test_*.py'"""

import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("usage_run", os.path.join(HERE, "usage", "run.py"))
run = importlib.util.module_from_spec(spec)
spec.loader.exec_module(run)


class DefaultTranscripts(unittest.TestCase):
    def test_xdg_state_home_wins(self):
        self.assertEqual(run.default_transcripts({"XDG_STATE_HOME": "/s"}), "/s/reeve/bench/transcripts/usage")

    def test_falls_back_to_local_state_outside_the_repo(self):
        got = run.default_transcripts({})
        self.assertEqual(got, os.path.join(os.path.expanduser("~"), ".local/state/reeve/bench/transcripts/usage"))
        self.assertFalse(os.path.abspath(got).startswith(os.path.dirname(HERE) + os.sep))


if __name__ == "__main__":
    unittest.main()

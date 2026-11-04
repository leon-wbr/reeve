"""Tests for redact.py. Run: python3 -m unittest discover -s bench -p 'test_*.py'"""

import json
import unittest

from redact import Redactor

R = Redactor(home="/Users/jdoe", terms=["Jane Doe", "Jane", "Doe", "jdoe"], limit=100)


class Text(unittest.TestCase):
    def test_home_dir_becomes_tilde(self):
        self.assertEqual(R.text("cat /Users/jdoe/.claude/x.json"), "cat ~/.claude/x.json")

    def test_home_dir_in_claude_project_slug(self):
        self.assertEqual(R.text("projects/-Users-jdoe-work-reeve"), "projects/~-work-reeve")

    def test_emails(self):
        self.assertEqual(R.text("by someone.else+tag@mail.example.org today"), "by <email> today")

    def test_tokens(self):
        for secret in [
            "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123",
            "sk-proj-ABCDEFGHIJKLMNOPQRSTUV",
            "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
            "github_pat_11ABCDEFG0123456789_abcdefghijklmnop",
            "cfoat_abcdefghijklmnopqrstuvwx",
            "cfort_abcdefghijklmnopqrstuvwx",
            "xoxb-123456789012-abcdefghijkl",
            "AKIAABCDEFGHIJKLMNOP",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTYifQ.c2lnbmF0dXJlLXZhbHVl",
        ]:
            self.assertEqual(R.text(f"token {secret} end"), "token <secret> end", secret)

    def test_bearer_header(self):
        self.assertEqual(R.text("Authorization: Bearer abc.def-123_xyz"), "Authorization: Bearer <secret>")

    def test_names_whole_words_any_case(self):
        self.assertEqual(R.text("Jane Doe and JANE wrote to jdoe"), "<user> and <user> wrote to <user>")
        self.assertEqual(R.text("Janet and Doenitz stay"), "Janet and Doenitz stay")

    def test_long_strings_truncated(self):
        out = R.text("x" * 250)
        self.assertTrue(out.startswith("x" * 100))
        self.assertIn("[truncated 150 chars]", out)
        self.assertLess(len(out), 150)


class Jsonl(unittest.TestCase):
    def test_walks_nested_values_and_keys_stay(self):
        line = json.dumps({"cwd": "/Users/jdoe/w", "message": {"content": [{"type": "tool_result", "content": "y" * 300}]}})
        out = json.loads(R.jsonl_line(line))
        self.assertEqual(out["cwd"], "~/w")
        self.assertIn("[truncated", out["message"]["content"][0]["content"])

    def test_non_json_line_redacted_as_text(self):
        self.assertEqual(R.jsonl_line("oops /Users/jdoe/x"), "oops ~/x")

    def test_numbers_and_nulls_untouched(self):
        self.assertEqual(json.loads(R.jsonl_line('{"a": 1, "b": null, "c": true}')), {"a": 1, "b": None, "c": True})


if __name__ == "__main__":
    unittest.main()

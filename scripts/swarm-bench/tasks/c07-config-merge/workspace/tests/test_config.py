import copy
import unittest

import config


class MergeTest(unittest.TestCase):
    def test_nested_override(self):
        cfg = config.load({"server": {"port": 9000}})
        self.assertEqual(cfg["server"], {"port": 9000, "hosts": ["localhost"]})

    def test_defaults_untouched(self):
        before = copy.deepcopy(config.DEFAULTS)
        config.load({"server": {"port": 9000}, "debug": True})
        self.assertEqual(config.DEFAULTS, before)

    def test_inputs_untouched(self):
        base = {"a": {"b": 1}}
        override = {"a": {"c": 2}}
        merged = config.deep_merge(base, override)
        self.assertEqual(merged, {"a": {"b": 1, "c": 2}})
        self.assertEqual(base, {"a": {"b": 1}})

    def test_lists_replace(self):
        cfg = config.load({"server": {"hosts": ["a", "b"]}})
        self.assertEqual(cfg["server"]["hosts"], ["a", "b"])

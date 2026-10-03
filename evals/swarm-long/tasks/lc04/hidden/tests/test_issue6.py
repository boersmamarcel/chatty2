"""Issue 6: template name normalisation."""

import os
import shutil
import tempfile
import unittest

from stencil import DictLoader, Environment, FileSystemLoader, TemplateNotFound
from stencil.loaders import split_template_path


class SplitTests(unittest.TestCase):

    def test_backslash(self):
        self.assertEqual(split_template_path("partials\\nav.html"), ["partials", "nav.html"])

    def test_dot_and_empty_segments(self):
        self.assertEqual(split_template_path("./a//b.html"), ["a", "b.html"])
        self.assertEqual(split_template_path("a/./b/"), ["a", "b"])
        self.assertEqual(split_template_path(".\\x\\.\\y.txt"), ["x", "y.txt"])

    def test_parent_segment_rejected(self):
        for name in ("../x.html", "a/../b.html", "a\\..\\b.html", ".."):
            with self.assertRaises(TemplateNotFound):
                split_template_path(name)

    def test_empty_rejected(self):
        for name in ("", "./", "//"):
            with self.assertRaises(TemplateNotFound):
                split_template_path(name)


class DictLoaderTests(unittest.TestCase):

    def setUp(self):
        self.loader = DictLoader({"a.html": "A", "dir/b.html": "B"})
        self.env = Environment(loader=self.loader)

    def test_normalised_lookup(self):
        self.assertEqual(self.loader.get_source(self.env, "./a.html")[0], "A")
        self.assertEqual(self.loader.get_source(self.env, "dir\\b.html")[0], "B")
        self.assertEqual(self.loader.get_source(self.env, "dir//./b.html")[0], "B")

    def test_parent_rejected(self):
        with self.assertRaises(TemplateNotFound):
            self.loader.get_source(self.env, "dir/../a.html")


class EnvironmentTests(unittest.TestCase):

    def setUp(self):
        self.env = Environment(loader=DictLoader({
            "a.html": "A",
            "dir/b.html": "B",
            "page.html": "[{% include './partials/nav.html' %}]",
            "partials/nav.html": "nav",
        }))

    def test_same_template_object(self):
        first = self.env.get_template("./a.html")
        self.assertIs(self.env.get_template("a.html"), first)
        self.assertEqual(first.name, "a.html")

    def test_backslash_name(self):
        template = self.env.get_template("dir\\b.html")
        self.assertEqual(template.name, "dir/b.html")
        self.assertIs(self.env.get_template("dir/b.html"), template)

    def test_include_with_dot(self):
        self.assertEqual(self.env.get_template("page.html").render(), "[nav]")

    def test_parent_rejected(self):
        with self.assertRaises(TemplateNotFound):
            self.env.get_template("../a.html")


class FileSystemTests(unittest.TestCase):

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="stencil-hidden-")
        self.root = os.path.join(self.tmp, "templates")
        os.makedirs(os.path.join(self.root, "partials"))
        with open(os.path.join(self.root, "partials", "nav.html"), "w") as f:
            f.write("nav")
        with open(os.path.join(self.root, "x.html"), "w") as f:
            f.write("x")
        with open(os.path.join(self.tmp, "secret.txt"), "w") as f:
            f.write("password")
        self.loader = FileSystemLoader(self.root)
        self.env = Environment(loader=self.loader)

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def test_forms(self):
        for name in ("partials/nav.html", "partials\\nav.html", "./partials//nav.html"):
            self.assertEqual(self.loader.get_source(self.env, name)[0], "nav")

    def test_parent_rejected_even_inside_root(self):
        with self.assertRaises(TemplateNotFound):
            self.loader.get_source(self.env, "../secret.txt")
        with self.assertRaises(TemplateNotFound):
            self.loader.get_source(self.env, "partials/../x.html")

    def test_cache_identity(self):
        self.assertIs(self.env.get_template("./x.html"), self.env.get_template("x.html"))


if __name__ == "__main__":
    unittest.main()

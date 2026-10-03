"""Loading templates from a folder."""

import os
import shutil
import tempfile
import unittest

from stencil import Environment, FileSystemLoader, TemplateNotFound


class FileSystemLoaderTests(unittest.TestCase):

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="stencil-test-")
        self.root = os.path.join(self.tmp, "templates")
        os.makedirs(os.path.join(self.root, "partials"))
        with open(os.path.join(self.root, "partials", "nav.html"), "w") as f:
            f.write("<nav>{{ title }}</nav>")
        with open(os.path.join(self.root, "page.html"), "w") as f:
            f.write("{% include 'partials/nav.html' %}<main></main>")
        with open(os.path.join(self.tmp, "secret.txt"), "w") as f:
            f.write("password")
        self.env = Environment(loader=FileSystemLoader(self.root))

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def test_plain_names(self):
        self.assertEqual(self.env.get_template("page.html").render(title="Home"),
                         "<nav>Home</nav><main></main>")

    def test_backslash_separator(self):
        self.assertEqual(self.env.get_template("partials\\nav.html").render(title="x"), "<nav>x</nav>")

    def test_parent_directory_is_refused(self):
        with self.assertRaises(TemplateNotFound):
            self.env.get_template("../secret.txt")

    def test_missing_template(self):
        with self.assertRaises(TemplateNotFound):
            self.env.get_template("nope.html")


if __name__ == "__main__":
    unittest.main()

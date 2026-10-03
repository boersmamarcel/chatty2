import unittest

from throttle.errors import InvalidKeyError
from throttle.keys import normalise_key, normalise_route


class Issue6RouteTest(unittest.TestCase):
    def test_examples(self):
        self.assertEqual(normalise_route("/v1/Orders/?page=2"), "/v1/orders")
        self.assertEqual(normalise_route("v1//users/42/"), "/v1/users/{id}")
        self.assertEqual(
            normalise_key("Acme", "/v1/files/3F2504E0-4F89-11D3-9A0C-0305E82C3301#x"),
            "acme:/v1/files/{id}")

    def test_query_and_fragment_dropped(self):
        self.assertEqual(normalise_route("/v1/orders?page=2&id=5"), "/v1/orders")
        self.assertEqual(normalise_route("/v1/orders#top"), "/v1/orders")
        self.assertEqual(normalise_route("/v1/orders#frag?x=1"), "/v1/orders")
        self.assertEqual(normalise_route("/v1/orders/17?x=/a/b"), "/v1/orders/{id}")

    def test_slashes(self):
        self.assertEqual(normalise_route("//v1///orders//"), "/v1/orders")
        self.assertEqual(normalise_route("/v1/orders/"), "/v1/orders")
        self.assertEqual(normalise_route("v1/orders"), "/v1/orders")

    def test_root_forms(self):
        for route in ("", "   ", "/", "///", "?a=1", "#x", None, "/?q"):
            self.assertEqual(normalise_route(route), "/", repr(route))

    def test_uuid_segments(self):
        self.assertEqual(
            normalise_route("/v1/users/123e4567-e89b-12d3-a456-426614174000/keys"),
            "/v1/users/{id}/keys")
        self.assertEqual(
            normalise_route("/v1/users/123E4567-E89B-12D3-A456-426614174000"),
            "/v1/users/{id}")

    def test_non_identifiers_kept(self):
        self.assertEqual(normalise_route("/v1/abc123/x1"), "/v1/abc123/x1")
        self.assertEqual(normalise_route("/v1/123e4567e89b12d3a456426614174000"),
                         "/v1/123e4567e89b12d3a456426614174000")
        self.assertEqual(normalise_route("/v1/123e4567-e89b-12d3-a456-42661417400"),
                         "/v1/123e4567-e89b-12d3-a456-42661417400")
        self.assertEqual(normalise_route("/v1/123g4567-e89b-12d3-a456-426614174000"),
                         "/v1/123g4567-e89b-12d3-a456-426614174000")

    def test_whitespace_and_case(self):
        self.assertEqual(normalise_route("  /V1/Users/7/  "), "/v1/users/{id}")

    def test_equivalent_routes_share_a_key(self):
        keys = {
            normalise_key("acme", "/v1/orders/"),
            normalise_key(" ACME ", "/v1/orders?page=2"),
            normalise_key("Acme", "//v1/orders"),
            normalise_key("acme", "/V1/Orders#section"),
        }
        self.assertEqual(keys, {"acme:/v1/orders"})

    def test_tenant_rules_unchanged(self):
        with self.assertRaises(InvalidKeyError):
            normalise_key("  ", "/v1/orders")
        self.assertEqual(normalise_key("Acme", "/"), "acme:/")


if __name__ == "__main__":
    unittest.main()

import base64
import json
import os
import tempfile
import unittest
from unittest import mock

import codex_auth


def jwt_with_claims(claims):
    payload = json.dumps(claims).encode("utf-8")
    encoded = base64.urlsafe_b64encode(payload).decode("ascii").rstrip("=")
    return f"header.{encoded}.signature"


class CodexAuthTests(unittest.TestCase):
    def test_decodes_jwt_claims_and_returns_default_for_invalid_tokens(self):
        token = jwt_with_claims({"client_id": "client-123", "nested": {"value": True}})

        self.assertEqual(codex_auth.jwt_claim(token, "client_id"), "client-123")
        self.assertEqual(codex_auth.jwt_claims("not-a-jwt"), {})
        self.assertEqual(codex_auth.jwt_claim("not-a-jwt", "missing", "fallback"), "fallback")

    def test_recovers_client_id_from_access_token_then_id_token_audience(self):
        self.assertEqual(
            codex_auth.client_id_from_tokens(jwt_with_claims({"client_id": "access-client"}), ""),
            "access-client",
        )
        self.assertEqual(
            codex_auth.client_id_from_tokens("", jwt_with_claims({"aud": "id-client"})),
            "id-client",
        )
        self.assertEqual(
            codex_auth.client_id_from_tokens("", jwt_with_claims({"aud": ["first-client", "second-client"]})),
            "first-client",
        )

        with self.assertRaisesRegex(RuntimeError, "client_id"):
            codex_auth.client_id_from_tokens("", jwt_with_claims({"aud": []}))

    def test_loads_codex_tokens_with_account_and_client_headers(self):
        id_token = jwt_with_claims(
            {
                "aud": "codex-client",
                "https://api.openai.com/auth": {"chatgpt_account_id": "account-from-id-token"},
            }
        )
        auth = self.write_auth_and_load(
            {
                "tokens": {
                    "access_token": "access-token",
                    "refresh_token": "refresh-token",
                    "id_token": id_token,
                }
            }
        )

        self.assertEqual(auth.token, "access-token")
        self.assertEqual(auth.refresh_token, "refresh-token")
        self.assertEqual(auth.account_id, "account-from-id-token")
        self.assertEqual(auth.client_id, "codex-client")
        self.assertEqual(auth.base_url, codex_auth.CHATGPT_CODEX_BASE_URL)
        self.assertEqual(auth.headers()["ChatGPT-Account-ID"], "account-from-id-token")

    def test_loads_openai_api_key_fallback(self):
        auth = self.write_auth_and_load({"OPENAI_API_KEY": "sk-local"})

        self.assertEqual(auth.token, "sk-local")
        self.assertIsNone(auth.refresh_token)
        self.assertEqual(auth.base_url, codex_auth.OPENAI_API_BASE_URL)
        self.assertNotIn("ChatGPT-Account-ID", auth.headers())

    def test_refresh_auth_updates_tokens_in_memory_without_persisting(self):
        auth = codex_auth.Auth("old-access", refresh_token="old-refresh", client_id="client-1")
        refreshed_id_token = jwt_with_claims(
            {"https://api.openai.com/auth": {"chatgpt_account_id": "account-1"}}
        )

        with mock.patch.object(
            codex_auth,
            "post_json",
            return_value={
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "id_token": refreshed_id_token,
            },
        ) as post_json:
            self.assertTrue(codex_auth.refresh_auth(auth))

        self.assertEqual(auth.token, "new-access")
        self.assertEqual(auth.refresh_token, "new-refresh")
        self.assertEqual(auth.account_id, "account-1")
        self.assertEqual(auth.client_id, "client-1")
        post_json.assert_called_once()

    def write_auth_and_load(self, data):
        with tempfile.NamedTemporaryFile("w", encoding="utf-8", delete=False) as f:
            json.dump(data, f)
            path = f.name

        try:
            return codex_auth.load_auth(path)
        finally:
            os.unlink(path)


if __name__ == "__main__":
    unittest.main()

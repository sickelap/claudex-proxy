import http.client
import io
import json
import threading
import unittest
import urllib.error
import urllib.parse
from unittest import mock

import codex_auth
import openai_api_proxy


class OpenAIAPIProxyTests(unittest.TestCase):
    def test_parse_args_defaults_to_localhost_port_8000(self):
        args = openai_api_proxy.parse_args([])

        self.assertEqual(args.host, "127.0.0.1")
        self.assertEqual(args.port, 8000)

    def test_forwards_requests_without_authorization_when_proxy_key_env_is_set(self):
        calls = []
        with mock.patch.dict("os.environ", {"OPENAI_API_PROXY_KEY": "secret"}):
            server, base_url = self.start_proxy(
                upstream_sender=lambda auth, body, stream: calls.append((body, stream))
                or openai_api_proxy.UpstreamResponse(200, b'{"ok":true}', "application/json"),
            )

            response = self.request(base_url + "/v1/responses", b"{}")

            self.assertEqual(response.status, 200)
            self.assertEqual(response.read(), b'{"ok":true}')
            response.close()
            self.assertEqual(calls, [(b"{}", False)])
            self.stop_proxy(server)

    def test_ignores_arbitrary_authorization_header_for_local_proxy_auth(self):
        calls = []
        server, base_url = self.start_proxy(
            upstream_sender=lambda auth, body, stream: calls.append((body, stream))
            or openai_api_proxy.UpstreamResponse(200, b'{"ok":true}', "application/json"),
        )

        response = self.request(base_url + "/v1/responses", b"{}", token="not-the-proxy-key")

        self.assertEqual(response.status, 200)
        self.assertEqual(response.read(), b'{"ok":true}')
        response.close()
        self.assertEqual(calls, [(b"{}", False)])
        self.stop_proxy(server)

    def test_rejects_invalid_json_without_forwarding_upstream(self):
        calls = []
        server, base_url = self.start_proxy(
            upstream_sender=lambda auth, body, stream: calls.append("upstream")
        )

        response = self.request(base_url + "/v1/responses", b'{"input":')

        self.assertEqual(response.status, 400)
        self.assertIn(b"invalid JSON", response.read())
        response.close()
        self.assertEqual(calls, [])
        self.stop_proxy(server)

    def test_rejects_unsupported_methods_and_paths_without_forwarding(self):
        calls = []
        server, base_url = self.start_proxy(
            upstream_sender=lambda auth, body, stream: calls.append("upstream")
        )

        get_response = self.request(base_url + "/v1/responses", method="GET")
        path_response = self.request(base_url + "/v1/chat/completions", b"{}")

        self.assertEqual(get_response.status, 405)
        self.assertEqual(path_response.status, 404)
        get_response.close()
        path_response.close()
        self.assertEqual(calls, [])
        self.stop_proxy(server)

    def test_forwards_non_streaming_json_body_and_status(self):
        request_body = b'{"model":"gpt-test","input":[]}'
        calls = []

        def upstream(auth, body, stream):
            calls.append((auth.token, body, stream))
            return openai_api_proxy.UpstreamResponse(201, b'{"id":"resp_1"}', "application/json")

        server, base_url = self.start_proxy(upstream_sender=upstream)

        response = self.request(base_url + "/v1/responses", request_body)

        self.assertEqual(response.status, 201)
        self.assertEqual(self.content_type(response), "application/json")
        self.assertEqual(response.read(), b'{"id":"resp_1"}')
        response.close()
        self.assertEqual(calls, [("token", request_body, False)])
        self.stop_proxy(server)

    def test_removes_max_output_tokens_from_opencode_request_and_preserves_other_fields(self):
        payload = {
            "model": "gpt-5.5",
            "input": [
                {"role": "developer", "content": "Use tools when needed."},
                {"role": "user", "content": [{"type": "input_text", "text": "hi"}]},
            ],
            "max_output_tokens": 32000,
            "store": False,
            "include": ["reasoning.encrypted_content"],
            "tools": [
                {
                    "type": "function",
                    "name": "read",
                    "description": "Read a file",
                    "parameters": {"type": "object", "properties": {}},
                }
            ],
            "tool_choice": "auto",
            "stream": True,
        }
        calls = []

        def upstream(auth, body, stream):
            calls.append((json.loads(body), stream))
            return openai_api_proxy.UpstreamResponse(200, b"", "text/event-stream")

        server, base_url = self.start_proxy(upstream_sender=upstream)

        response = self.request(
            base_url + "/v1/responses",
            json.dumps(payload).encode("utf-8"),
        )

        self.assertEqual(response.status, 200)
        response.close()
        expected = dict(payload)
        expected.pop("max_output_tokens")
        self.assertEqual(calls, [(expected, True)])
        self.stop_proxy(server)

    def test_preserves_original_request_bytes_when_no_normalization_is_needed(self):
        request_body = b'{ "model": "gpt-test", "input": [], "store": false }'
        calls = []

        def upstream(auth, body, stream):
            calls.append(body)
            return openai_api_proxy.UpstreamResponse(200, b"{}", "application/json")

        server, base_url = self.start_proxy(upstream_sender=upstream)

        response = self.request(base_url + "/v1/responses", request_body)

        self.assertEqual(response.status, 200)
        response.close()
        self.assertEqual(calls, [request_body])
        self.stop_proxy(server)

    def test_forwards_streaming_sse_bytes_without_conversion(self):
        upstream_body = [b"event: response.output_text.delta\n", b'data: {"delta":"hi"}\n\n']

        def upstream(auth, body, stream):
            self.assertTrue(stream)
            return openai_api_proxy.UpstreamResponse(200, upstream_body, "text/event-stream")

        server, base_url = self.start_proxy(upstream_sender=upstream)

        response = self.request(base_url + "/v1/responses", b'{"stream":true}')

        self.assertEqual(response.status, 200)
        self.assertEqual(self.content_type(response), "text/event-stream")
        self.assertEqual(response.read(), b"".join(upstream_body))
        response.close()
        self.stop_proxy(server)

    def test_closes_upstream_when_streaming_client_disconnects(self):
        for disconnect_error in (BrokenPipeError, ConnectionResetError):
            with self.subTest(disconnect_error=disconnect_error.__name__):
                closed = []
                handler = object.__new__(openai_api_proxy.ProxyHandler)
                handler.send_response = lambda status: None
                handler.send_header = lambda name, value: None
                handler.end_headers = lambda: None

                class DisconnectedWriter:
                    def write(self, chunk):
                        raise disconnect_error()

                    def flush(self):
                        pass

                handler.wfile = DisconnectedWriter()
                upstream_response = openai_api_proxy.UpstreamResponse(
                    200,
                    [b"event: response.output_text.delta\n"],
                    "text/event-stream",
                    close=lambda: closed.append(True),
                )

                handler.write_upstream_response(upstream_response, stream=True)

                self.assertEqual(closed, [True])

    def test_refreshes_auth_and_retries_once_after_unauthorized_upstream_response(self):
        auth = codex_auth.Auth("expired", refresh_token="refresh", client_id="client")
        calls = []

        def upstream(auth, body, stream):
            calls.append(auth.token)
            if len(calls) == 1:
                raise urllib.error.HTTPError(
                    openai_api_proxy.RESPONSES_URL,
                    401,
                    "Unauthorized",
                    hdrs=None,
                    fp=io.BytesIO(b"expired"),
                )
            return openai_api_proxy.UpstreamResponse(200, b'{"ok":true}', "application/json")

        def refresh(auth):
            auth.token = "refreshed"
            return True

        server, base_url = self.start_proxy(
            auth_loader=lambda: auth,
            upstream_sender=upstream,
            refresh_func=refresh,
        )

        response = self.request(base_url + "/v1/responses", b"{}")

        self.assertEqual(response.status, 200)
        self.assertEqual(response.read(), b'{"ok":true}')
        response.close()
        self.assertEqual(calls, ["expired", "refreshed"])
        self.stop_proxy(server)

    def test_passes_through_non_refreshable_upstream_http_errors(self):
        def upstream(auth, body, stream):
            raise urllib.error.HTTPError(
                openai_api_proxy.RESPONSES_URL,
                400,
                "Bad Request",
                hdrs={"Content-Type": "application/json"},
                fp=io.BytesIO(b'{"error":"bad"}'),
            )

        server, base_url = self.start_proxy(upstream_sender=upstream)

        response = self.request(base_url + "/v1/responses", b"{}")

        self.assertEqual(response.status, 400)
        self.assertEqual(response.read(), b'{"error":"bad"}')
        response.close()
        self.stop_proxy(server)

    def test_returns_bad_gateway_for_network_failures_without_http_response(self):
        def upstream(auth, body, stream):
            raise urllib.error.URLError("connection failed")

        server, base_url = self.start_proxy(upstream_sender=upstream)

        response = self.request(base_url + "/v1/responses", b"{}")

        self.assertEqual(response.status, 502)
        self.assertIn(b"upstream request failed", response.read())
        response.close()
        self.stop_proxy(server)

    def start_proxy(self, auth_loader=None, upstream_sender=None, refresh_func=None):
        server = openai_api_proxy.build_server(
            "127.0.0.1",
            0,
            auth_loader=auth_loader or (lambda: codex_auth.Auth("token")),
            upstream_sender=upstream_sender
            or (lambda auth, body, stream: openai_api_proxy.UpstreamResponse(200, b"{}", "application/json")),
            refresh_func=refresh_func or (lambda auth: False),
        )
        thread = threading.Thread(target=server.serve_forever)
        thread.daemon = True
        thread.start()
        server.test_thread = thread
        host, port = server.server_address
        return server, f"http://{host}:{port}"

    def stop_proxy(self, server):
        server.shutdown()
        server.server_close()
        server.test_thread.join(timeout=2)

    def request(self, url, body=None, method="POST", token=None):
        parsed = urllib.parse.urlsplit(url)
        headers = {}
        if body is not None:
            headers["Content-Type"] = "application/json"
        if token is not None:
            headers["Authorization"] = f"Bearer {token}"
        path = parsed.path or "/"
        if parsed.query:
            path += "?" + parsed.query
        connection = http.client.HTTPConnection(parsed.hostname, parsed.port, timeout=2)
        connection.request(method, path, body=body, headers=headers)
        response = connection.getresponse()
        response.connection = connection
        return response

    def content_type(self, response):
        return response.getheader("Content-Type").split(";", 1)[0]


if __name__ == "__main__":
    unittest.main()

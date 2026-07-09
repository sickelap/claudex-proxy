#!/usr/bin/env python3
import argparse
import json
import sys
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from codex_auth import load_auth, refresh_auth

RESPONSES_URL = "https://chatgpt.com/backend-api/codex/responses"


class UpstreamResponse:
    def __init__(self, status, body, content_type="application/json", close=None):
        self.status = status
        self.body = body
        self.content_type = content_type
        self._close = close

    def iter_body(self):
        if isinstance(self.body, str):
            yield self.body.encode("utf-8")
        elif isinstance(self.body, (bytes, bytearray)):
            yield bytes(self.body)
        else:
            yield from self.body

    def close(self):
        if self._close:
            self._close()


def normalize_request_body(body, payload):
    if not isinstance(payload, dict) or "max_output_tokens" not in payload:
        return body
    normalized = dict(payload)
    normalized.pop("max_output_tokens")
    return json.dumps(normalized).encode("utf-8")


def send_upstream(auth, body, stream):
    headers = auth.headers()
    headers["Accept"] = "text/event-stream" if stream else "application/json"
    request = urllib.request.Request(
        auth.base_url + "/responses",
        data=body,
        headers=headers,
        method="POST",
    )
    response = urllib.request.urlopen(request, timeout=120)
    return UpstreamResponse(
        response.status,
        response,
        response.headers.get_content_type(),
        close=response.close,
    )


def build_server(
    host,
    port,
    auth_loader=load_auth,
    upstream_sender=send_upstream,
    refresh_func=refresh_auth,
):
    class ProxyServer(ThreadingHTTPServer):
        daemon_threads = True

    server = ProxyServer((host, port), ProxyHandler)
    server.auth_loader = auth_loader
    server.upstream_sender = upstream_sender
    server.refresh_func = refresh_func
    return server


class ProxyHandler(BaseHTTPRequestHandler):
    server_version = "OpenAIAPIProxy/1.0"

    def do_POST(self):
        if self.path != "/v1/responses":
            self.write_json_error(
                404, "unsupported path: only POST /v1/responses is supported"
            )
            return

        try:
            body = self.read_body()
            payload = json.loads(body.decode("utf-8"))
        except json.JSONDecodeError:
            self.write_json_error(400, "invalid JSON request body")
            return
        except UnicodeDecodeError:
            self.write_json_error(400, "request body must be UTF-8 JSON")
            return

        stream = isinstance(payload, dict) and payload.get("stream") is True
        upstream_body = normalize_request_body(body, payload)

        try:
            auth = self.server.auth_loader()
            upstream_response = self.send_with_refresh_retry(
                auth, upstream_body, stream
            )
        except urllib.error.HTTPError as error:
            self.write_upstream_http_error(error)
            return
        except urllib.error.URLError:
            self.write_json_error(502, "upstream request failed")
            return

        self.write_upstream_response(upstream_response, stream)

    def do_GET(self):
        self.write_unsupported_method()

    def do_PUT(self):
        self.write_unsupported_method()

    def do_PATCH(self):
        self.write_unsupported_method()

    def do_DELETE(self):
        self.write_unsupported_method()

    def write_unsupported_method(self):
        self.write_json_error(
            405, "unsupported method: only POST /v1/responses is supported"
        )

    def read_body(self):
        content_length = self.headers.get("Content-Length")
        if not content_length:
            return b""
        return self.rfile.read(int(content_length))

    def send_with_refresh_retry(self, auth, body, stream):
        try:
            return self.server.upstream_sender(auth, body, stream)
        except urllib.error.HTTPError as error:
            if error.code in (401, 403) and self.server.refresh_func(auth):
                self.close_http_error(error)
                return self.server.upstream_sender(auth, body, stream)
            raise

    def write_upstream_response(self, upstream_response, stream):
        try:
            content_type = (
                "text/event-stream" if stream else upstream_response.content_type
            )
            self.send_response(upstream_response.status)
            self.send_header("Content-Type", content_type)
            if isinstance(upstream_response.body, (bytes, bytearray, str)):
                length = len(
                    upstream_response.body.encode("utf-8")
                    if isinstance(upstream_response.body, str)
                    else upstream_response.body
                )
                self.send_header("Content-Length", str(length))
            self.end_headers()
            try:
                for chunk in upstream_response.iter_body():
                    if chunk:
                        self.wfile.write(chunk)
                        self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass
        finally:
            upstream_response.close()

    def write_upstream_http_error(self, error):
        try:
            body = error.read()
            content_type = "application/json"
            if error.headers:
                content_type = error.headers.get("Content-Type", content_type)
            self.send_response(error.code)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        finally:
            self.close_http_error(error)

    def close_http_error(self, error):
        if error.fp:
            error.fp.close()
        error.close()

    def write_json_error(self, status, message):
        body = json.dumps({"error": message}).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        return


def parse_args(argv):
    parser = argparse.ArgumentParser(
        description="Local OpenAI Responses proxy backed by Codex auth"
    )
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", default=8000, type=int)
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    server = build_server(args.host, args.port)
    print(
        f"openai-api-proxy listening on http://{args.host}:{args.port}", file=sys.stderr
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print(file=sys.stderr)
    finally:
        server.server_close()


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"fatal: {error}", file=sys.stderr)
        sys.exit(1)

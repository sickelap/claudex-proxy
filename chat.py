#!/usr/bin/env python3
import json
import os
import sys
import urllib.error
import urllib.request

from codex_auth import load_auth, refresh_auth

try:
    import readline
except ImportError:
    readline = None


def post_stream_text(url, payload, headers, timeout=120):
    body = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(url, data=body, headers=headers, method="POST")
    parts = []
    final_response = None

    with urllib.request.urlopen(request, timeout=timeout) as response:
        event_name = None
        data_lines = []
        for raw_line in response:
            line = raw_line.decode("utf-8").rstrip("\r\n")
            if not line:
                final_response = handle_stream_event(event_name, data_lines, parts) or final_response
                event_name = None
                data_lines = []
            elif line.startswith("event:"):
                event_name = line[6:].strip()
            elif line.startswith("data:"):
                data_lines.append(line[5:].lstrip())

        final_response = handle_stream_event(event_name, data_lines, parts) or final_response

    text = "".join(parts)
    if text:
        return text.strip()
    return output_text(final_response or {})


def handle_stream_event(event_name, data_lines, parts):
    if not data_lines:
        return None

    data = "\n".join(data_lines)
    if data == "[DONE]":
        return None

    try:
        event = json.loads(data)
    except json.JSONDecodeError:
        return None

    event_type = event.get("type") or event_name
    if event_type in {"response.output_text.delta", "response.refusal.delta"}:
        parts.append(event.get("delta", ""))
    elif event_type == "response.completed":
        return event.get("response") or event
    elif event_type in {"error", "response.failed"}:
        raise RuntimeError(stream_error_message(event))
    return None


def stream_error_message(event):
    error = event.get("error") or (event.get("response") or {}).get("error") or event
    if isinstance(error, dict):
        return error.get("message") or error.get("detail") or json.dumps(error)
    return str(error)


def ask(auth, messages):
    payload = {
        "model": os.environ.get("OPENAI_MODEL", "gpt-5.5"),
        "input": messages,
        "store": False,
        "stream": True,
    }
    url = auth.base_url + "/responses"
    headers = auth.headers()
    headers["Accept"] = "text/event-stream"

    try:
        return post_stream_text(url, payload, headers)
    except urllib.error.HTTPError as e:
        if e.code in (401, 403) and refresh_auth(auth):
            headers = auth.headers()
            headers["Accept"] = "text/event-stream"
            return post_stream_text(url, payload, headers)
        detail = e.read().decode("utf-8", "replace")
        raise RuntimeError(f"HTTP {e.code}: {detail}") from None


def output_text(response):
    if response.get("output_text"):
        return response["output_text"]

    parts = []
    for item in response.get("output", []):
        if item.get("type") != "message":
            continue
        for content in item.get("content", []):
            if content.get("type") == "output_text":
                parts.append(content.get("text", ""))
            elif content.get("type") == "refusal":
                parts.append(content.get("refusal", ""))
    return "".join(parts).strip()


def setup_readline():
    if readline and hasattr(readline, "set_auto_history"):
        readline.set_auto_history(False)


def add_history(line):
    if readline and line.strip():
        readline.add_history(line)


def main():
    setup_readline()
    auth = load_auth()
    messages = []
    print("codex-chat: /exit or Ctrl-D to quit")

    while True:
        try:
            prompt = input("> ")
        except EOFError:
            print()
            return
        except KeyboardInterrupt:
            print()
            continue

        command = prompt.strip()
        if not command:
            continue
        if command in {"/exit", "/quit"}:
            return

        add_history(prompt)
        messages.append({"role": "user", "content": prompt})

        try:
            answer = ask(auth, messages)
        except Exception as e:
            messages.pop()
            print(f"error: {e}", file=sys.stderr)
            continue

        if not answer:
            answer = "[no text output]"
        print(answer)
        messages.append({"role": "assistant", "content": answer})


if __name__ == "__main__":
    try:
        main()
    except Exception as e:
        print(f"fatal: {e}", file=sys.stderr)
        sys.exit(1)

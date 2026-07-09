import base64
import json
import os
import urllib.request

AUTH_PATH = os.path.expanduser("~/.codex/auth.json")
CHATGPT_CODEX_BASE_URL = "https://chatgpt.com/backend-api/codex"
OPENAI_API_BASE_URL = "https://api.openai.com/v1"
REFRESH_URL = "https://auth.openai.com/oauth/token"


class Auth:
    def __init__(self, token, refresh_token=None, account_id=None, client_id=None, base_url=CHATGPT_CODEX_BASE_URL):
        self.token = token
        self.refresh_token = refresh_token
        self.account_id = account_id
        self.client_id = client_id
        self.base_url = base_url.rstrip("/")

    def headers(self):
        headers = {
            "Authorization": f"Bearer {self.token}",
            "Content-Type": "application/json",
            "Accept": "application/json",
            "User-Agent": "codex-chat-python",
        }
        if self.account_id:
            headers["ChatGPT-Account-ID"] = self.account_id
        return headers


def load_auth(path=AUTH_PATH):
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)

    tokens = data.get("tokens") or {}
    access_token = tokens.get("access_token")
    if access_token:
        account_id = tokens.get("account_id") or jwt_claim(
            tokens.get("id_token", ""), "https://api.openai.com/auth", {}
        ).get("chatgpt_account_id")
        client_id = client_id_from_tokens(access_token, tokens.get("id_token", ""))
        return Auth(access_token, tokens.get("refresh_token"), account_id, client_id)

    api_key = data.get("OPENAI_API_KEY")
    if api_key:
        return Auth(api_key, base_url=OPENAI_API_BASE_URL)

    raise RuntimeError(f"no usable token found in {path}")


def jwt_claims(token):
    try:
        payload = token.split(".")[1]
        payload += "=" * (-len(payload) % 4)
        return json.loads(base64.urlsafe_b64decode(payload))
    except Exception:
        return {}


def jwt_claim(token, name, default=None):
    return jwt_claims(token).get(name, default)


def client_id_from_tokens(access_token, id_token):
    client_id = jwt_claim(access_token, "client_id")
    if client_id:
        return client_id

    audience = jwt_claim(id_token, "aud")
    if isinstance(audience, str) and audience:
        return audience
    if isinstance(audience, list) and audience and audience[0]:
        return audience[0]

    raise RuntimeError("could not recover OAuth client_id from Codex auth tokens")


def post_json(url, payload, headers, timeout=120):
    body = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(url, data=body, headers=headers, method="POST")
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def refresh_auth(auth):
    if not auth.refresh_token:
        return False

    payload = {
        "client_id": auth.client_id,
        "grant_type": "refresh_token",
        "refresh_token": auth.refresh_token,
    }
    try:
        data = post_json(REFRESH_URL, payload, {"Content-Type": "application/json", "Accept": "application/json"}, 60)
    except Exception:
        return False

    auth.token = data.get("access_token") or auth.token
    auth.refresh_token = data.get("refresh_token") or auth.refresh_token
    auth.account_id = auth.account_id or jwt_claim(
        data.get("id_token", ""), "https://api.openai.com/auth", {}
    ).get("chatgpt_account_id")
    auth.client_id = auth.client_id or client_id_from_tokens(data.get("access_token", ""), data.get("id_token", ""))
    return bool(data.get("access_token"))

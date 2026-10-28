#!/usr/bin/env python3
"""Apply bilingual public bot metadata. Credentials are read only from the environment."""
import argparse
import json
import os
from pathlib import Path
import secrets
import sys
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check-only", action="store_true")
    parser.add_argument("--photo", type=Path, default=ROOT / "assets/avatar.jpg")
    args = parser.parse_args()
    token = os.environ.get("TELEGRAM_BOT_TOKEN", "").strip()
    if not token:
        raise SystemExit("TELEGRAM_BOT_TOKEN is required")

    def call(method, body=None, photo=None):
        if photo is None:
            data = json.dumps(body or {}).encode()
            content_type = "application/json"
        else:
            boundary = "Napominalochka" + secrets.token_hex(16)
            parts = []
            for key, value in (body or {}).items():
                parts.append((f'--{boundary}\r\nContent-Disposition: form-data; name="{key}"\r\n\r\n{value}\r\n').encode())
            parts.append((f'--{boundary}\r\nContent-Disposition: form-data; name="avatar"; filename="avatar.jpg"\r\nContent-Type: image/jpeg\r\n\r\n').encode())
            parts.extend([photo.read_bytes(), f"\r\n--{boundary}--\r\n".encode()])
            data = b"".join(parts)
            content_type = "multipart/form-data; boundary=" + boundary
        req = Request("https://api.telegram.org/bot" + token + "/" + method, data=data, headers={"Content-Type": content_type})
        try:
            with urlopen(req, timeout=30) as response:
                result = json.load(response)
        except HTTPError as error:
            try:
                result = json.loads(error.read())
            except (ValueError, OSError):
                raise SystemExit(f"{method}: HTTP {error.code}") from None
        except URLError:
            raise SystemExit(f"{method}: network request failed") from None
        if not result.get("ok"):
            raise SystemExit(f"{method}: Telegram error {result.get('error_code')}: {result.get('description', '')}")
        return result["result"]

    me = call("getMe")
    if me.get("username") != "napominalozhka_bot":
        raise SystemExit("Token belongs to a different bot; no changes applied")
    print("Verified bot: @" + me["username"])
    profile = json.loads((ROOT / "ops/bot-profile.json").read_text())
    for language in ["", "ru", "en"]:
        if args.check_only:
            for method in ["getMyName", "getMyDescription", "getMyShortDescription"]:
                result = call(method, {"language_code": language})
                print(method, language or "default", json.dumps(result, ensure_ascii=False))
            continue
        texts = profile[language or "ru"]
        for method, field, limit in [("setMyName", "name", 64), ("setMyDescription", "description", 512), ("setMyShortDescription", "short_description", 120)]:
            if len(texts[field]) > limit:
                raise SystemExit(field + " exceeds Telegram's limit")
            call(method, {field: texts[field], "language_code": language})
            print(method, language or "default", "OK")
    if args.check_only:
        print("menu", json.dumps(call("getChatMenuButton"), ensure_ascii=False))
        return
    call("setMyProfilePhoto", {"photo": json.dumps({"type": "static", "photo": "attach://avatar"})}, photo=args.photo)
    print("Avatar OK")
    url = os.environ.get("WEB_APP_URL", "").strip()
    if url:
        if not url.startswith("https://"):
            raise SystemExit("WEB_APP_URL must use HTTPS")
        call("setChatMenuButton", {"menu_button": {"type": "web_app", "text": "Напоминания", "web_app": {"url": url}}})
        print("Mini App menu OK")


if __name__ == "__main__":
    main()

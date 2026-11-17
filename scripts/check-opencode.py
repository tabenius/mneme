#!/usr/bin/env python3
"""GET-only preflight. Prints contract metadata, never credentials/session bodies."""
import argparse
import base64
import json
from pathlib import Path
import urllib.error
import urllib.parse
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def inspect(url, password, describe_legacy=False):
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme not in ("http", "https") or parsed.hostname not in (
        "127.0.0.1", "localhost", "::1"
    ) or parsed.username or parsed.password:
        raise ValueError("Expected a loopback HTTP(S) URL without credentials")
    opener = urllib.request.build_opener(NoRedirect, urllib.request.ProxyHandler({}))
    auth = base64.b64encode(("opencode:" + password).encode()).decode()
    endpoints = {}
    documents = {}
    for path in ("/api/info", "/openapi.json", "/global/health", "/doc"):
        req = urllib.request.Request(url.rstrip("/") + path, headers={"Authorization": "Basic " + auth})
        try:
            with opener.open(req, timeout=15) as response:
                payload = response.read(8 * 1024 * 1024 + 1)
                if len(payload) > 8 * 1024 * 1024:
                    endpoints[path] = "response-too-large"
                    continue
                try:
                    documents[path] = json.loads(payload)
                    endpoints[path] = "json"
                except (ValueError, UnicodeDecodeError):
                    endpoints[path] = "not-json"
        except urllib.error.HTTPError as error:
            endpoints[path] = "http-" + str(error.code)
        except (OSError, urllib.error.URLError):
            endpoints[path] = "unreachable"
    required = ["/api/session", "/api/session/{sessionID}/message",
                "/api/experimental/session/{sessionID}/log", "/api/event"]
    schema = documents.get("/openapi.json", {})
    paths = schema.get("paths", {}) if isinstance(schema, dict) else {}
    missing = [path for path in required if not isinstance(paths.get(path, {}).get("get"), dict)]
    info = documents.get("/api/info", documents.get("/global/health", {}))
    version = info.get("version") if isinstance(info, dict) else None
    result = {"compatible": not missing and isinstance(documents.get("/api/info"), dict),
              "version": version, "endpoints": endpoints, "missingRequiredGetPaths": missing}
    if describe_legacy:
        legacy = documents.get("/doc", {})
        contracts = {path: item["get"] for path, item in legacy.get("paths", {}).items()
                     if path in ("/session", "/session/{sessionID}/message", "/event", "/global/health") and "get" in item}
        schemas = legacy.get("components", {}).get("schemas", {})
        refs = set()

        def visit(value):
            if isinstance(value, dict):
                ref = value.get("$ref", "")
                if ref.startswith("#/components/schemas/") and ref not in refs:
                    refs.add(ref)
                    visit(schemas.get(ref.rsplit("/", 1)[-1], {}))
                for child in value.values():
                    visit(child)
            elif isinstance(value, list):
                for child in value:
                    visit(child)

        visit(contracts)
        result["legacyContracts"] = contracts
        result["legacySchemas"] = {ref.rsplit("/", 1)[-1]: schemas[ref.rsplit("/", 1)[-1]] for ref in sorted(refs)}
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--password-file", type=Path, required=True)
    parser.add_argument("--password-format", choices=("json", "plain"), default="json")
    parser.add_argument("--describe-legacy", action="store_true", help="Print selected API schemas only, never session responses")
    args = parser.parse_args()
    raw = args.password_file.read_text()
    password = json.loads(raw)["password"] if args.password_format == "json" else raw.rstrip("\r\n")
    if not isinstance(password, str) or not password:
        raise SystemExit("Credential file needs a nonempty password")
    result = inspect(args.url, password, args.describe_legacy)
    print(json.dumps(result, indent=2))
    return 0 if result["compatible"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Black-box MCP stdio lifecycle test for the built container image.

The test intentionally supplies no credentials and calls only the embedded,
read-only repo_map tool. It validates protocol framing, notification silence,
post-error recovery, clean EOF shutdown, structured stderr, and the absence of
credential-shaped values without rejecting redacted documentation strings.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from typing import Any, NoReturn


PROTOCOL_VERSION = "2025-06-18"
REQUIRED_TOOLS = {
    "repo_map",
    "node_status",
    "cloudflare_dns_upsert",
    "cloudflare_dns_delete",
}
EXPECTED_RESPONSE_IDS = {1, 2, 3, 4, 5, 6}

CRASH_MARKERS = (
    "panicked at",
    "thread 'main' panicked",
    "backtrace:",
)
CREDENTIAL_PATTERNS = (
    (
        "GitHub personal access token",
        re.compile(r"\bghp_[A-Za-z0-9]{20,}\b"),
    ),
    (
        "Fiducia live/test credential",
        re.compile(r"\bfdc_(?:live|test)_[A-Za-z0-9_.-]{8,}\b", re.IGNORECASE),
    ),
    (
        "private key",
        re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----", re.IGNORECASE),
    ),
    (
        "bearer credential",
        # Documentation such as "bearer auth" and explicitly redacted
        # "Bearer ***" examples are not credentials. Real bearer values are
        # expected to be substantially longer than an ordinary word.
        re.compile(
            r"\bauthorization\s*:\s*bearer\s+"
            r"(?!\*{3}(?:\s|$)|<redacted>(?:\s|$))"
            r"[A-Za-z0-9._~+/=-]{12,}",
            re.IGNORECASE,
        ),
    ),
    (
        "assigned secret value",
        re.compile(
            r"\b(?:fiducia_api_key|cloudflare_api_token|fiducia_internal_secret)"
            r"\s*=\s*"
            r"(?!false\b|true\b|unset\b|none\b|not[- ]configured\b|"
            r"\*{3}(?:\s|$)|<redacted>(?:\s|$))"
            r"[^\s,;}'\"]{8,}",
            re.IGNORECASE,
        ),
    ),
)


def fail(message: str, *, stdout: str = "", stderr: str = "") -> NoReturn:
    print(f"MCP stdio smoke failed: {message}", file=sys.stderr)
    if stdout:
        print("--- stdout ---", file=sys.stderr)
        print(stdout, file=sys.stderr)
    if stderr:
        print("--- stderr ---", file=sys.stderr)
        print(stderr, file=sys.stderr)
    raise SystemExit(1)


def response_by_id(messages: list[dict[str, Any]], request_id: int) -> dict[str, Any]:
    matches = [message for message in messages if message.get("id") == request_id]
    if len(matches) != 1:
        fail(f"expected exactly one response for id={request_id}, found {len(matches)}")
    return matches[0]


def successful_response(
    messages: list[dict[str, Any]], request_id: int, method: str
) -> dict[str, Any]:
    response = response_by_id(messages, request_id)
    if "result" not in response or "error" in response:
        fail(f"{method} did not succeed")
    result = response["result"]
    if not isinstance(result, dict):
        fail(f"{method} returned a non-object result")
    return result


def parse_protocol_stdout(stdout: str, stderr: str) -> list[dict[str, Any]]:
    messages: list[dict[str, Any]] = []
    for line_number, line in enumerate(stdout.splitlines(), start=1):
        if not line.strip():
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError as error:
            fail(
                f"stdout line {line_number} is not JSON-RPC: {error}",
                stdout=stdout,
                stderr=stderr,
            )
        if not isinstance(message, dict) or message.get("jsonrpc") != "2.0":
            fail(
                f"stdout line {line_number} is not a JSON-RPC 2.0 object",
                stdout=stdout,
                stderr=stderr,
            )
        messages.append(message)
    if not messages:
        fail("server emitted no protocol messages", stdout=stdout, stderr=stderr)
    return messages


def validate_structured_stderr(stderr: str, stdout: str) -> None:
    """Require one JSON log object per non-empty stderr line.

    The normal MCP transport reserves stdout for protocol frames. Requiring
    structured stderr catches accidental plaintext debugging while still
    allowing the embedded repo-map documentation to appear inside a JSON field.
    """

    for line_number, line in enumerate(stderr.splitlines(), start=1):
        if not line.strip():
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError as error:
            fail(
                f"stderr line {line_number} is not structured JSON: {error}",
                stdout=stdout,
                stderr=stderr,
            )
        if not isinstance(record, dict):
            fail(
                f"stderr line {line_number} is not a JSON object",
                stdout=stdout,
                stderr=stderr,
            )
        if not isinstance(record.get("level"), str) or not isinstance(
            record.get("message"), str
        ):
            fail(
                f"stderr line {line_number} lacks string level/message fields",
                stdout=stdout,
                stderr=stderr,
            )


def validate_no_sensitive_output(stdout: str, stderr: str) -> None:
    combined = f"{stdout}\n{stderr}"
    lowered = combined.lower()
    crash_hits = [marker for marker in CRASH_MARKERS if marker in lowered]
    credential_hits = [
        label for label, pattern in CREDENTIAL_PATTERNS if pattern.search(combined)
    ]
    if crash_hits or credential_hits:
        findings = [
            *(f"crash marker: {marker}" for marker in crash_hits),
            *(f"credential shape: {label}" for label in credential_hits),
        ]
        fail(
            f"process output contains forbidden marker(s): {', '.join(findings)}",
            stdout=stdout,
            stderr=stderr,
        )


def run(image: str) -> None:
    frames = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "fiducia-mcp-ci", "version": "1.0"},
            },
        },
        {
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {},
        },
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
        {
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "repo_map", "arguments": {}},
        },
        {"jsonrpc": "2.0", "id": 4, "method": "ping", "params": {}},
        {
            "jsonrpc": "2.0",
            "id": 5,
            "method": "fiducia/definitely-unknown",
            "params": {},
        },
        # A second ping after the typed protocol error proves the same process
        # remains responsive rather than merely producing an error frame.
        {"jsonrpc": "2.0", "id": 6, "method": "ping", "params": {}},
    ]
    payload = "".join(
        f"{json.dumps(frame, separators=(',', ':'))}\n" for frame in frames
    )

    command = ["docker", "run", "--rm", "-i", image, "--log-filter=debug"]
    try:
        completed = subprocess.run(
            command,
            input=payload,
            text=True,
            capture_output=True,
            timeout=20,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        fail(
            "container did not exit cleanly after stdin EOF",
            stdout=_as_text(error.stdout),
            stderr=_as_text(error.stderr),
        )

    stdout = completed.stdout
    stderr = completed.stderr
    if completed.returncode != 0:
        fail(
            f"container did not exit cleanly (status {completed.returncode})",
            stdout=stdout,
            stderr=stderr,
        )

    messages = parse_protocol_stdout(stdout, stderr)
    validate_structured_stderr(stderr, stdout)
    validate_no_sensitive_output(stdout, stderr)

    response_ids = [message.get("id") for message in messages]
    if any(request_id is None for request_id in response_ids):
        fail(
            "server emitted an unsolicited or notification response without an id",
            stdout=stdout,
            stderr=stderr,
        )
    if set(response_ids) != EXPECTED_RESPONSE_IDS or len(response_ids) != len(
        EXPECTED_RESPONSE_IDS
    ):
        fail(
            f"expected exactly response ids {sorted(EXPECTED_RESPONSE_IDS)}, got {response_ids}",
            stdout=stdout,
            stderr=stderr,
        )

    initialize = successful_response(messages, 1, "initialize")
    if initialize.get("protocolVersion") != PROTOCOL_VERSION:
        fail(
            "initialize did not negotiate the requested supported protocol version",
            stdout=stdout,
            stderr=stderr,
        )
    if not isinstance(initialize.get("capabilities"), dict):
        fail("initialize omitted capabilities", stdout=stdout, stderr=stderr)
    server_info = initialize.get("serverInfo", {})
    if not isinstance(server_info, dict) or not server_info.get("name") or not server_info.get(
        "version"
    ):
        fail("initialize omitted serverInfo name/version", stdout=stdout, stderr=stderr)

    tools_response = successful_response(messages, 2, "tools/list")
    tools = tools_response.get("tools")
    if not isinstance(tools, list):
        fail("tools/list did not return a tools array", stdout=stdout, stderr=stderr)
    tool_names = []
    for tool in tools:
        if not isinstance(tool, dict):
            fail("tools/list contains a non-object tool", stdout=stdout, stderr=stderr)
        name = tool.get("name")
        if not isinstance(name, str) or not name:
            fail("tools/list contains a tool without a name", stdout=stdout, stderr=stderr)
        if not isinstance(tool.get("description"), str) or not tool["description"].strip():
            fail(f"tool {name} has no description", stdout=stdout, stderr=stderr)
        input_schema = tool.get("inputSchema")
        if not isinstance(input_schema, dict) or input_schema.get("type") != "object":
            fail(f"tool {name} has no object input schema", stdout=stdout, stderr=stderr)
        tool_names.append(name)
    if len(tool_names) != len(set(tool_names)):
        fail("tools/list contains duplicate names", stdout=stdout, stderr=stderr)
    missing_tools = sorted(REQUIRED_TOOLS - set(tool_names))
    if missing_tools:
        fail(
            f"tools/list is missing required tools: {', '.join(missing_tools)}",
            stdout=stdout,
            stderr=stderr,
        )

    repo_map = successful_response(messages, 3, "repo_map")
    if repo_map.get("isError") is True:
        fail("repo_map unexpectedly returned isError=true", stdout=stdout, stderr=stderr)
    content = repo_map.get("content")
    if not isinstance(content, list) or not content:
        fail("repo_map returned no MCP content", stdout=stdout, stderr=stderr)
    text = "\n".join(
        item.get("text", "")
        for item in content
        if isinstance(item, dict) and item.get("type") == "text"
    )
    if "fiducia" not in text.lower():
        fail("repo_map content does not identify Fiducia", stdout=stdout, stderr=stderr)

    successful_response(messages, 4, "ping before protocol error")

    unknown = response_by_id(messages, 5)
    error = unknown.get("error")
    if not isinstance(error, dict) or error.get("code") != -32601:
        fail(
            "unknown method did not fail with JSON-RPC method-not-found (-32601)",
            stdout=stdout,
            stderr=stderr,
        )

    successful_response(messages, 6, "ping after protocol error")

    print(
        "MCP stdio lifecycle passed "
        f"({len(messages)} responses, {len(tools)} unique tools, clean_exit=true)"
    )


def _as_text(value: str | bytes | None) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return value


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} <container-image>")
    run(sys.argv[1])

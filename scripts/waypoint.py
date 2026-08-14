#!/usr/bin/env python3

"""waypoint - CLI interface for waypointd server.

Talks to the waypointd HTTP API (plus the few public non-API routes) using
only the Python standard library. Every invocation is a single request pair,
so the script is "online": it never touches the database directly.

    waypoint list --starred
    waypoint add https://example.com/ --tags dev --keyword ex

Global options (before the subcommand):
    -u/--url URL       server base URL (default: $WAYPOINTD_SERVE_HOST +
                       $WAYPOINTD_SERVE_PORT, else http://localhost:8080)
    -t/--token TOKEN   bearer token for /api/* (default: $WAYPOINTD_TOKEN,
                       then $WAYPOINTD_SERVE_TOKEN, then $WAYPOINTD_READ_TOKEN)
    -J/--json          print raw JSON instead of the human-readable view
    -T/--timeout N     per-request timeout in seconds (default 30)

Every option has both a long flag and a short flag (e.g. -J/--json); the
short flags are scoped to the parser they are declared on, so `-t` means
--token here but --tags on `add`. Short flags are single letters: lowercase
is the common first letter, and an uppercase twin marks the "opposite"
variant (e.g. -A/--archived vs -a/--active, -d/--created-after vs
-D/--created-before).

Exit codes: 0 success, 1 server/validation error, 2 connection error.
"""

import argparse
import http.client
import json
import os
import socket
import sys
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, BinaryIO, Callable, TextIO, cast

DEFAULT_URL: str = "http://localhost:8080"

# Hosts the server accepts as bind-all values; a client can't connect to
# them, so they're mapped to `localhost` when building a base URL.
_BIND_ALL_HOSTS: set[str] = {"0.0.0.0", "::", "[::]"}


# ── Colored output ──────────────────────────────────────────────────────────
# ANSI SGR codes render on every modern terminal; the only real exception is
# the legacy Windows console, which needs VT processing switched on first
# (via ctypes — still the standard library, no third-party deps). Colors are
# dropped when output is redirected or piped, when TERM=dumb, or when the
# NO_COLOR env var is set (per the NO_COLOR spec, even an empty value).

_STYLE_CODES: dict[str, str] = {
	"bold": "1",
	"dim": "2",
	"red": "31",
	"green": "32",
	"yellow": "33",
	"blue": "34",
	"magenta": "35",
	"cyan": "36",
}


def _enable_windows_vt() -> bool:
	"""Switch on ANSI/VT processing for a Windows console via ctypes, so
	ANSI codes work on Windows 10+ the same way they do on macOS/Linux/BSD.
	Returns False on legacy consoles, where color is simply skipped."""
	try:
		import ctypes
	except ImportError:
		return False
	windll = getattr(ctypes, "windll", None)
	if windll is None:
		return False
	kernel32 = getattr(windll, "kernel32", None)
	if kernel32 is None:
		return False
	handle = kernel32.GetStdHandle(-11)  # STD_OUTPUT_HANDLE
	mode = ctypes.c_uint32()
	if not kernel32.GetConsoleMode(handle, ctypes.byref(mode)):
		return False
	kernel32.SetConsoleMode(handle, mode.value | 0x0004)  # ENABLE_VIRTUAL_TERMINAL_PROCESSING
	return True


def _color_enabled() -> bool:
	# The NO_COLOR spec: presence of the variable (even empty) disables colors.
	if os.environ.get("NO_COLOR") is not None:
		return False
	if os.environ.get("TERM") == "dumb":
		return False
	# Color only reaches a terminal; piped/redirected output stays plain.
	if not (sys.stdout.isatty() or sys.stderr.isatty()):
		return False
	if os.name == "nt":
		return _enable_windows_vt()
	return True


_COLOR_ON: bool = _color_enabled()


def paint(text: str, *styles: str) -> str:
	"""Wrap `text` in ANSI SGR codes for the given styles ("red", "bold",
	"cyan", ...). Returns `text` unchanged when color output is disabled."""
	if not _COLOR_ON or not styles:
		return text
	codes = ";".join(_STYLE_CODES[style] for style in styles)
	return f"\033[{codes}m{text}\033[0m"


def default_url() -> str:
	"""Base URL for the client, in priority order.

	1. The `-u/--url` flag.
	2. `WAYPOINTD_SERVE_HOST` + `WAYPOINTD_SERVE_PORT` — the server's own
	   bind envs, so the CLI can be pointed at a server using the exact
	   variables that started it (bind-all hosts become `localhost`).
	3. The hard-coded `DEFAULT_URL` (server defaults `localhost`/`8080`).
	"""
	host: str = os.environ.get("WAYPOINTD_SERVE_HOST") or "localhost"
	host = "localhost" if host in _BIND_ALL_HOSTS else host
	if host.startswith(("http://", "https://")):
		host = host.split("://", 1)[1]
	if ":" in host and not host.startswith("["):
		host = f"[{host}]"
	port: str = os.environ.get("WAYPOINTD_SERVE_PORT") or "8080"
	return f"http://{host}:{port}"


class CommandError(Exception):
	"""A user-facing failure: HTTP error status or bad local arguments."""

	message: str
	code: str | None
	status: int | None

	def __init__(self, message: str, code: str | None = None, status: int | None = None) -> None:
		super().__init__(message)
		self.message = message
		self.code = code
		self.status = status

	def print(self, stream: TextIO = sys.stderr) -> None:
		if self.code:
			stream.write(paint(f"error [{self.code}]: {self.message}", "red") + "\n")
		else:
			stream.write(paint(f"error: {self.message}", "red") + "\n")


class NetworkError(Exception):
	"""Could not reach the server at all (DNS, refused, timeout).

	Carries the URL that failed so the diagnostic can point at the exact
	config the CLI is using instead of dumping a raw urllib error.
	"""

	message: str
	base_url: str
	cause: BaseException

	def __init__(self, message: str, base_url: str, cause: BaseException) -> None:
		super().__init__(message)
		self.message = message
		self.base_url = base_url
		self.cause = cause

	def _port(self) -> int | str:
		try:
			return urllib.parse.urlsplit(self.base_url).port or 8080
		except ValueError:
			return "?"

	def advice(self) -> str:
		"""A human-readable diagnosis with concrete next steps."""
		low: str = self.message.lower()
		if isinstance(self.cause, ConnectionRefusedError) or "connection refused" in low:
			reason: str = f"nothing is listening at {self.base_url}"
			fixes: list[str] = [
				f"start a waypointd bound to that port, e.g. WAYPOINTD_SERVE_PORT={self._port()} waypointd",
				"point the CLI at the real server: -u URL, or WAYPOINTD_SERVE_HOST + WAYPOINTD_SERVE_PORT",
				"check the server log for the host:port it actually bound to",
			]
		elif isinstance(self.cause, socket.timeout) or "timed out" in low:
			reason = f"{self.base_url} did not answer within the timeout"
			fixes = [
				"the server may be overloaded or hung — retry in a moment",
				"give it more time with --timeout N (per-request seconds)",
			]
		elif isinstance(self.cause, socket.gaierror) or "name or service not known" in low or "getaddrinfo" in low:
			reason = f"the host in {self.base_url} does not resolve"
			fixes = [
				"check the host part of the URL (-u, or WAYPOINTD_SERVE_HOST)",
				"make sure it is a name this machine can resolve (DNS or /etc/hosts)",
			]
		else:
			reason = f"{self.base_url} could not be reached"
			fixes = [
				"verify the base URL (-u, or WAYPOINTD_SERVE_HOST + WAYPOINTD_SERVE_PORT)",
				"if urllib is told to use a proxy (HTTP_PROXY / HTTPS_PROXY / ALL_PROXY), make sure it is up",
			]
		return (
			f"error: cannot reach waypointd at {self.base_url}\n"
			f"  reason: {reason}\n"
			"  how to fix:\n"
			+ "\n".join(f"    - {fix}" for fix in fixes)
		)

	def print(self, stream: TextIO = sys.stderr) -> None:
		lines: list[str] = self.advice().split("\n")
		lines[0] = paint(lines[0], "red")
		stream.write("\n".join(lines) + "\n")


class Response:
	status: int
	headers: dict[str, str]
	raw: bytes
	text: str
	data: Any

	def __init__(self, status: int, headers: dict[str, str], raw: bytes) -> None:
		self.status = status
		self.headers = headers
		self.raw = raw
		self.text = raw.decode("utf-8", "replace")
		self.data = None
		if raw:
			try:
				self.data = json.loads(self.text)
			except ValueError:
				pass

	@property
	def ok(self) -> bool:
		return 200 <= self.status < 300

	def _content_type(self) -> str:
		ct: str = self.headers.get("content-type") or self.headers.get("Content-Type") or ""
		return ct.lower()

	def _looks_like_html(self) -> bool:
		return "text/html" in self._content_type() or self.text.lstrip().startswith(("<", "<!"))

	def error_message(self) -> str:
		if isinstance(self.data, dict):
			msg: Any = self.data.get("error") or self.data.get("message")
			if msg:
				return msg
		if self._looks_like_html():
			return (
				f"HTTP {self.status}: the server at this address answered with an HTML "
				"page, not waypointd JSON — is a waypointd actually listening here "
				"(wrong host/port, or a proxy in the way)?"
			)
		if self.text.strip():
			return self.text.strip()
		return f"HTTP {self.status}"

	def error_code(self) -> str | None:
		if isinstance(self.data, dict):
			return self.data.get("code")
		return None


class _NoRedirect(urllib.request.HTTPRedirectHandler):
	"""Expose redirect status codes instead of following them silently."""

	def redirect_request(self, req, fp, code, msg, headers, newurl):
		return None


class WaypointdClient:
	base_url: str
	token: str | None
	timeout: int
	_opener: urllib.request.OpenerDirector

	def __init__(self, base_url: str, token: str | None = None, timeout: int = 30) -> None:
		self.base_url = base_url.rstrip("/")
		self.token = token
		self.timeout = timeout
		self._opener = urllib.request.build_opener(_NoRedirect)

	def request(
		self,
		method: str,
		path: str,
		params: dict[str, str] | None = None,
		body: dict | None = None,
	) -> Response:
		url: str = self.base_url + path
		if params:
			url += "?" + urllib.parse.urlencode(params)
		headers: dict[str, str] = {}
		if path.startswith("/api/") and path != "/api/auth/signin" and self.token:
			headers["Authorization"] = f"Bearer {self.token}"
		data: bytes | None = None
		if body is not None:
			data = json.dumps(body).encode("utf-8")
			headers["Content-Type"] = "application/json"
		req: urllib.request.Request = urllib.request.Request(url, data=data, headers=headers, method=method)
		try:
			resp: http.client.HTTPResponse | urllib.error.HTTPError = self._opener.open(req, timeout=self.timeout)
		except urllib.error.HTTPError as e:
			resp = e
		except (urllib.error.URLError, OSError) as e:
			reason: Any = getattr(e, "reason", e)
			raise NetworkError(str(reason), self.base_url, reason)
		return Response(cast(int, resp.status), dict(resp.headers.items()), resp.read())


def ensure_ok(resp: Response) -> None:
	if resp.ok:
		return
	raise CommandError(resp.error_message(), code=resp.error_code(), status=resp.status)


def split_csv(s: str) -> list[str]:
	# Comma-separated lists appear in several flags (-g/--tags, -a/--add-tags);
	# keep a single tolerant parser that drops empties and stray whitespace.
	return [p.strip() for p in s.split(",") if p.strip()]


def parse_ids(s: str) -> list[int]:
	return [int(p) for p in split_csv(s)]


def pjson(obj: Any) -> None:
	# Pretty-print JSON for --json output; default=str renders datetimes etc.
	print(json.dumps(obj, indent=2, default=str))


def print_table(cols: list[str], rows: list[list[str]]) -> None:
	# Column-aligned plain table: computes the widest cell per column, prints
	# a cyan/bold header, a dashed separator, then the rows.
	rows = [[str(c) for c in row] for row in rows]
	widths: list[int] = [len(c) for c in cols]
	row: list[str]
	for row in rows:
		i: int
		cell: str
		for i, cell in enumerate(row):
			if i < len(widths):
				widths[i] = max(widths[i], len(cell))

	def fmt(cells: list[str]) -> str:
		return "  ".join(cell.ljust(widths[i]) for i, cell in enumerate(cells)).rstrip()

	print(paint(fmt(cols), "cyan", "bold"))
	print("  ".join("-" * w for w in widths))
	for row in rows:
		print(fmt(row))


def truncate_url(url: str, limit: int = 30) -> str:
	# Shorten long URLs for the table view so they don't blow up the column
	# width: keep the first `limit` characters and append "...".
	if len(url) <= limit:
		return url
	return url[:limit] + "..."


def emit_bookmarks(rows: list[dict], total: str | None, args: argparse.Namespace) -> None:
	# Shared renderer for list/search: JSON when --json, otherwise a table
	# with a dim "N shown, M total" footer (x-total-count from the server).
	if args.json:
		pjson(rows)
		return
	if not rows:
		print(paint("no bookmarks", "yellow"))
	else:
		table: list[list[str]] = [
			[
				str(b.get("id", "")),
				b.get("keyword") or "",
				b.get("domain") or "",
				b.get("title") or "",
				truncate_url(b.get("url") or ""),
				",".join(b.get("tags") or []),
			]
			for b in rows
		]
		print_table(["ID", "KW", "DOMAIN", "TITLE", "URL", "TAGS"], table)
	if total is not None:
		print(paint(f"{len(rows)} shown, {total} total", "dim"))


def emit_bookmark(b: dict, args: argparse.Namespace) -> None:
	# Single-bookmark renderer (add/get/update results): JSON or a key/value
	# listing. Only non-empty fields are shown so the output stays compact.
	if args.json:
		pjson(b)
		return
	print(paint(f"#{b.get('id')}  {b.get('title') or b.get('url')}", "bold"))
	key: str
	label: str
	for key, label in (
		("url", "url"),
		("description", "description"),
		("domain", "domain"),
		("keyword", "keyword"),
		("category_name", "category"),
		("note", "note"),
		("starred", "starred"),
		("is_archived", "archived"),
		("visit_count", "visits"),
		("last_visited_at", "last visited"),
		("created_at", "created"),
		("updated_at", "updated"),
		("trashed_at", "trashed"),
		("tags", "tags"),
	):
		v: Any = b.get(key)
		if v not in (None, "", False):
			print(f"  {paint(label + ':', 'cyan')} {v}")


# ---------------------------------------------------------------------------
# Argument builders
# ---------------------------------------------------------------------------


def add_list_filters(sp: argparse.ArgumentParser, paging: bool = True, cursor: bool = True) -> None:
	# Filter options shared by `list`, `bulk-delete`, and `search`. Short
	# flags: -c/--category, -C/--category-id, -g/--tag, -k/--keyword,
	# -s/--starred, -A/--archived, -a/--active, -x/--trash.
	sp.add_argument("-c", "--category")
	sp.add_argument("-C", "--category-id", type=int)
	sp.add_argument("-g", "--tag")
	sp.add_argument("-k", "--keyword")
	sp.add_argument("-s", "--starred", action="store_true")
	# --archived and --active are the two ends of the same switch, so they
	# are mutually exclusive.
	g: argparse._MutuallyExclusiveGroup = sp.add_mutually_exclusive_group()
	g.add_argument("-A", "--archived", action="store_true", help="only archived bookmarks")
	g.add_argument("-a", "--active", action="store_true", help="only active bookmarks")
	sp.add_argument("-x", "--trash", action="store_true", help="list trashed bookmarks")
	# Date windows: each field has an "-after" and "-before" half. Short
	# flags pair a lowercase letter (the field's initial) with its uppercase
	# twin for the "before" half: -d/-D (created), -u/-U (updated),
	# -v/-V (visited), -t/-T (trashed).
	for short, name in (("d", "created"), ("u", "updated"), ("v", "visited"), ("t", "trashed")):
		sp.add_argument(f"-{short}", f"--{name}-after")
		sp.add_argument(f"-{short.upper()}", f"--{name}-before")
	if paging:
		# -l/--limit and -o/--offset page the result set.
		sp.add_argument("-l", "--limit", type=int)
		sp.add_argument("-o", "--offset", type=int)
	if cursor:
		# -z/--cursor is the opaque server-side pagination token.
		sp.add_argument("-z", "--cursor")


def add_new_fields(sp: argparse.ArgumentParser) -> None:
	# Optional bookmark fields for `add`. Short flags: -t/--title,
	# -d/--description, -c/--category, -g/--tags, -k/--keyword,
	# -n/--note, -f/--favicon, -m/--thumbnail, -F/--favicon-mode,
	# -M/--thumbnail-mode, -s/--starred, -A/--archived.
	sp.add_argument("-t", "--title")
	sp.add_argument("-d", "--description")
	sp.add_argument("-c", "--category")
	sp.add_argument("-g", "--tags", metavar="A,B")
	sp.add_argument("-k", "--keyword")
	sp.add_argument("-n", "--note")
	sp.add_argument("-f", "--favicon")
	sp.add_argument("-m", "--thumbnail")
	sp.add_argument("-F", "--favicon-mode", choices=["auto", "default", "fetch"])
	sp.add_argument("-M", "--thumbnail-mode", choices=["auto", "default", "fetch"])
	sp.add_argument("-s", "--starred", action="store_true")
	sp.add_argument("-A", "--archived", action="store_true")


def add_update_fields(sp: argparse.ArgumentParser) -> None:
	# Editable fields for `update` and `bulk-update`. Short flags:
	# -t/--title, -U/--url, -d/--description, -c/--category,
	# -g/--tags, -a/--add-tags, -R/--remove-tags, -k/--keyword,
	# -n/--note, -f/--favicon, -m/--thumbnail, -F/--favicon-mode,
	# -M/--thumbnail-mode, -s/--starred, -S/--no-starred,
	# -A/--archived, -N/--no-archived, -r/--refresh.
	sp.add_argument("-t", "--title")
	sp.add_argument("-U", "--url")
	sp.add_argument("-d", "--description")
	sp.add_argument("-c", "--category")
	sp.add_argument("-g", "--tags", metavar="A,B", help="full replacement")
	sp.add_argument("-a", "--add-tags", metavar="A,B")
	sp.add_argument("-R", "--remove-tags", metavar="A,B")
	sp.add_argument("-k", "--keyword", help="empty string clears it")
	sp.add_argument("-n", "--note")
	sp.add_argument("-f", "--favicon")
	sp.add_argument("-m", "--thumbnail")
	sp.add_argument("-F", "--favicon-mode", choices=["auto", "default", "fetch"])
	sp.add_argument("-M", "--thumbnail-mode", choices=["auto", "default", "fetch"])
	sp.add_argument("-s", "--starred", action="store_true")
	sp.add_argument("-S", "--no-starred", action="store_true")
	sp.add_argument("-A", "--archived", action="store_true")
	sp.add_argument("-N", "--no-archived", action="store_true")
	sp.add_argument("-r", "--refresh", action="store_true", help="re-fetch favicon/thumbnail")


def collect_list_params(args: argparse.Namespace) -> dict[str, str]:
	# Turn the shared filter args (see add_list_filters) into query params.
	# The "archived" key carries both --archived (true) and --active (false);
	# --starred/--trash map to their own keys.
	params: dict[str, str] = {}
	if getattr(args, "category", None):
		params["category"] = args.category
	if getattr(args, "category_id", None) is not None:
		params["category_id"] = str(args.category_id)
	if getattr(args, "tag", None):
		params["tag"] = args.tag
	if getattr(args, "keyword", None):
		params["keyword"] = args.keyword
	if getattr(args, "starred", False):
		params["starred"] = "true"
	if getattr(args, "archived", False):
		params["archived"] = "true"
	if getattr(args, "active", False):
		params["archived"] = "false"
	if getattr(args, "trash", False):
		params["trash"] = "true"
	name: str
	for name in ("created", "updated", "visited", "trashed"):
		v: Any = getattr(args, f"{name}_after", None)
		if v:
			params[f"{name}_after"] = v
		v = getattr(args, f"{name}_before", None)
		if v:
			params[f"{name}_before"] = v
	if getattr(args, "limit", None):
		params["limit"] = str(args.limit)
	if getattr(args, "offset", None) is not None:
		params["offset"] = str(args.offset)
	if getattr(args, "cursor", None):
		params["cursor"] = args.cursor
	return params


def collect_new(args: argparse.Namespace) -> dict:
	# Build the POST /api/bookmarks body from the add flags. URL is required;
	# the rest are included only when the user actually passed them.
	body: dict = {"url": args.url}
	key: str
	for key in ("title", "description", "category", "keyword", "note",
	            "favicon", "thumbnail", "favicon_mode", "thumbnail_mode"):
		v: Any = getattr(args, key, None)
		if v is not None:
			body[key] = v
	if getattr(args, "tags", None) is not None:
		body["tags"] = split_csv(args.tags)
	if getattr(args, "starred", False):
		body["starred"] = True
	if getattr(args, "archived", False):
		body["is_archived"] = True
	return body


def collect_update(args: argparse.Namespace) -> dict:
	# Build the PUT/PATCH body from the update flags. Only fields the user
	# passed make it in, so a partial update never clobbers what was omitted;
	# --starred/--archived and their --no-* twins set boolean true/false.
	u: dict = {}
	if getattr(args, "title", None) is not None:
		u["title"] = args.title
	if getattr(args, "url", None) is not None:
		u["url"] = args.url
	if getattr(args, "description", None) is not None:
		u["description"] = args.description
	if getattr(args, "category", None) is not None:
		u["category"] = args.category
	if getattr(args, "tags", None) is not None:
		u["tags"] = split_csv(args.tags)
	if getattr(args, "add_tags", None) is not None:
		u["add_tags"] = split_csv(args.add_tags)
	if getattr(args, "remove_tags", None) is not None:
		u["remove_tags"] = split_csv(args.remove_tags)
	if getattr(args, "keyword", None) is not None:
		u["keyword"] = args.keyword
	if getattr(args, "note", None) is not None:
		u["note"] = args.note
	if getattr(args, "favicon", None) is not None:
		u["favicon"] = args.favicon
	if getattr(args, "thumbnail", None) is not None:
		u["thumbnail"] = args.thumbnail
	if getattr(args, "favicon_mode", None) is not None:
		u["favicon_mode"] = args.favicon_mode
	if getattr(args, "thumbnail_mode", None) is not None:
		u["thumbnail_mode"] = args.thumbnail_mode
	if getattr(args, "starred", False):
		u["starred"] = True
	if getattr(args, "no_starred", False):
		u["starred"] = False
	if getattr(args, "archived", False):
		u["is_archived"] = True
	if getattr(args, "no_archived", False):
		u["is_archived"] = False
	if getattr(args, "refresh", False):
		u["refresh"] = True
	return u


# ---------------------------------------------------------------------------
# Commands
#
# Every command is `cmd_<name>(client, args)`; the parser wires a `func`
# default onto each subparser and `dispatch` calls it. Read-only commands
# share the request + `emit_*` helpers below; mutating commands print a
# short green one-liner on success. Commands exit 0 on success, 1 on a
# server/validation error (CommandError), 2 on a connection failure
# (NetworkError).
# ---------------------------------------------------------------------------


def cmd_list(client: WaypointdClient, args: argparse.Namespace) -> None:
	params: dict[str, str] = collect_list_params(args)
	if args.all:
		# --all walks the cursor pagination until the last page, so it needs
		# a non-trash filter set and an explicit page size.
		if params.get("trash"):
			raise CommandError("--all does not work with --trash (trash has no cursor)")
		page_size: int = args.limit or 200
		params["limit"] = str(page_size)
		rows: list[dict] = []
		total: str | None = None
		while True:
			resp: Response = client.request("GET", "/api/bookmarks", params=params)
			ensure_ok(resp)
			if total is None:
				total = resp.headers.get("x-total-count")
			page: list[dict] = resp.data or []
			rows.extend(page)
			nxt: str | None = resp.headers.get("x-next-cursor")
			if not nxt or len(page) < page_size:
				break
			params.pop("offset", None)
			params["cursor"] = nxt
		emit_bookmarks(rows, total, args)
	else:
		resp = client.request("GET", "/api/bookmarks", params=params)
		ensure_ok(resp)
		emit_bookmarks(resp.data or [], resp.headers.get("x-total-count"), args)


def cmd_add(client: WaypointdClient, args: argparse.Namespace) -> None:
	# POST /api/bookmarks; the URL is required, everything else optional.
	resp: Response = client.request("POST", "/api/bookmarks", body=collect_new(args))
	ensure_ok(resp)
	emit_bookmark(resp.data, args)


def cmd_get(client: WaypointdClient, args: argparse.Namespace) -> None:
	# GET a single bookmark by id and render it.
	resp: Response = client.request("GET", f"/api/bookmarks/{args.id}")
	ensure_ok(resp)
	emit_bookmark(resp.data, args)


def cmd_note(client: WaypointdClient, args: argparse.Namespace) -> None:
	# The note endpoint returns raw text/plain, so print it verbatim (no
	# table/json wrapping) so the note can be piped or copied.
	resp: Response = client.request("GET", f"/api/bookmarks/{args.id}/note")
	ensure_ok(resp)
	sys.stdout.write(resp.text)


def cmd_update(client: WaypointdClient, args: argparse.Namespace) -> None:
	# PUT /api/bookmarks/{id}: collect_update includes only the fields the
	# user passed, so an empty payload is a user error worth calling out.
	u: dict = collect_update(args)
	if not u:
		raise CommandError("nothing to update; pass at least one field")
	resp: Response = client.request("PUT", f"/api/bookmarks/{args.id}", body=u)
	ensure_ok(resp)
	emit_bookmark(resp.data, args)


def cmd_delete(client: WaypointdClient, args: argparse.Namespace) -> None:
	# DELETE moves the bookmark to the trash unless --purge is set; the
	# response body is empty, so report what happened ourselves.
	params: dict[str, str] = {"purge": "true"} if args.purge else {"purge": "false"}
	resp: Response = client.request("DELETE", f"/api/bookmarks/{args.id}", params=params)
	ensure_ok(resp)
	if args.json:
		pjson({})
	print(paint("deleted bookmark " + str(args.id) + (" (purged)" if args.purge else " (moved to trash)"), "green"))


def cmd_restore(client: WaypointdClient, args: argparse.Namespace) -> None:
	# POST /api/bookmarks/{id}/restore pulls a bookmark back out of the trash.
	resp: Response = client.request("POST", f"/api/bookmarks/{args.id}/restore")
	ensure_ok(resp)
	print(paint(f"restored bookmark {args.id}", "green"))


def cmd_check_one(client: WaypointdClient, args: argparse.Namespace) -> None:
	# Liveness check for a single bookmark: GET /api/bookmarks/{id}/check
	# returns a status string (alive/dead/skipped), mapped to colored text.
	resp: Response = client.request("GET", f"/api/bookmarks/{args.id}/check")
	ensure_ok(resp)
	if args.json:
		pjson(resp.data)
		return
	status: str = resp.data.get("status")
	if status == "dead":
		print(paint(f"dead: {resp.data.get('reason')}", "red"))
	elif status == "skipped":
		print(paint("skipped (non-http(s) url)", "yellow"))
	else:
		print(paint("alive", "green"))


def cmd_bulk_update(client: WaypointdClient, args: argparse.Namespace) -> None:
	# PATCH /api/bookmarks applies one update to many ids in a single request.
	ids: list[int] = parse_ids(args.ids)
	if not ids:
		raise CommandError("--ids requires at least one id")
	u: dict = collect_update(args)
	if not u:
		raise CommandError("nothing to update; pass at least one field")
	resp: Response = client.request("PATCH", "/api/bookmarks", body={"ids": ids, "update": u})
	ensure_ok(resp)
	if args.json:
		pjson(resp.data)
		return
	print(paint("updated: " + str(resp.data.get("updated") or []), "green"))
	if resp.data.get("skipped"):
		print(paint("skipped (missing or trashed): " + str(resp.data["skipped"]), "yellow"))


def cmd_bulk_delete(client: WaypointdClient, args: argparse.Namespace) -> None:
	# DELETE /api/bookmarks takes either an explicit --ids list or the same
	# list/search filters as `list`; --dry-run reports without deleting.
	filters: dict[str, str] = collect_list_params(args)
	filters.pop("limit", None)
	filters.pop("offset", None)
	filters.pop("cursor", None)
	if args.ids and filters:
		raise CommandError("pass --ids OR filters, not both")
	params: dict[str, str] = {}
	if args.ids:
		params["ids"] = args.ids
	elif filters:
		params.update(filters)
	else:
		raise CommandError("pass --ids or at least one filter")
	if args.dry_run:
		params["dry_run"] = "true"
	resp: Response = client.request("DELETE", "/api/bookmarks", params=params)
	ensure_ok(resp)
	if args.json:
		pjson(resp.data)
		return
	d: dict = resp.data or {}
	verb: str = "would remove" if args.dry_run else "removed"
	print(paint(f"{verb} {d.get('removed')} bookmark(s): {d.get('ids') or []}", "green"))


def cmd_empty_trash(client: WaypointdClient, args: argparse.Namespace) -> None:
	# DELETE /api/trash purges everything in the trash, optionally only items
	# trashed before --before; --dry-run previews the ids.
	params: dict[str, str] = {}
	if args.before:
		params["before"] = args.before
	if args.dry_run:
		params["dry_run"] = "true"
	resp: Response = client.request("DELETE", "/api/trash", params=params)
	ensure_ok(resp)
	if args.json:
		pjson(resp.data)
		return
	d: dict = resp.data or {}
	verb: str = "would purge" if args.dry_run else "purged"
	print(paint(f"{verb} {d.get('removed')} trashed bookmark(s): {d.get('ids') or []}", "green"))


def cmd_categories(client: WaypointdClient, args: argparse.Namespace) -> None:
	# GET /api/categories: id/name table, or raw JSON with -J.
	resp: Response = client.request("GET", "/api/categories")
	ensure_ok(resp)
	if args.json:
		pjson(resp.data)
		return
	rows: list[list[str]] = [[str(c.get("id", "")), c.get("name") or ""] for c in (resp.data or [])]
	if rows:
		print_table(["ID", "NAME"], rows)
	else:
		print("no categories")


def cmd_category_rename(client: WaypointdClient, args: argparse.Namespace) -> None:
	# PUT /api/categories/{id} with the new name.
	resp: Response = client.request("PUT", f"/api/categories/{args.id}", body={"name": args.name})
	ensure_ok(resp)
	print(paint(f"renamed category {args.id} to {args.name}", "green"))


def cmd_category_delete(client: WaypointdClient, args: argparse.Namespace) -> None:
	# DELETE /api/categories/{id}; the server reassigns its bookmarks.
	resp: Response = client.request("DELETE", f"/api/categories/{args.id}")
	ensure_ok(resp)
	print(paint(f"deleted category {args.id}", "green"))


def cmd_tags(client: WaypointdClient, args: argparse.Namespace) -> None:
	# GET /api/tags returns name+count pairs for every tag in use.
	resp: Response = client.request("GET", "/api/tags")
	ensure_ok(resp)
	if args.json:
		pjson(resp.data)
		return
	rows: list[list[str]] = [[t.get("name") or "", str(t.get("count", ""))] for t in (resp.data or [])]
	if rows:
		print_table(["NAME", "COUNT"], rows)
	else:
		print("no tags")


def cmd_tag_rename(client: WaypointdClient, args: argparse.Namespace) -> None:
	# Tags live in the URL path, so the old name must be percent-encoded.
	quoted: str = urllib.parse.quote(args.name, safe="")
	resp: Response = client.request("PUT", f"/api/tags/{quoted}", body={"name": args.new_name})
	ensure_ok(resp)
	print(paint(f"renamed tag {args.name} to {args.new_name}", "green"))


def cmd_tag_delete(client: WaypointdClient, args: argparse.Namespace) -> None:
	quoted: str = urllib.parse.quote(args.name, safe="")
	resp: Response = client.request("DELETE", f"/api/tags/{quoted}")
	ensure_ok(resp)
	print(paint(f"deleted tag {args.name}", "green"))


def cmd_search(client: WaypointdClient, args: argparse.Namespace) -> None:
	# GET /api/search: full-text query + optional category/tag/keyword scopes.
	params: dict[str, str] = {"q": args.q}
	if args.category:
		params["category"] = args.category
	if args.tag:
		params["tag"] = args.tag
	if args.keyword:
		params["keyword"] = args.keyword
	if args.limit:
		params["limit"] = str(args.limit)
	if args.archived:
		params["archived"] = "true"
	elif args.active:
		params["archived"] = "false"
	resp: Response = client.request("GET", "/api/search", params=params)
	ensure_ok(resp)
	emit_bookmarks(resp.data or [], resp.headers.get("x-total-count"), args)


def cmd_import(client: WaypointdClient, args: argparse.Namespace) -> None:
	# POST /api/import uploads a Netscape bookmark export (or raw HTML); "-"
	# reads from stdin so it composes with curl/pipes. Optional default tags,
	# category, and --archive to import everything as archived.
	if args.file == "-":
		content: str = sys.stdin.read()
	else:
		f: TextIO
		with open(args.file, encoding="utf-8") as f:
			content = f.read()
	if not content.strip():
		raise CommandError("file is empty")
	body: dict = {"content": content}
	if args.tags:
		body["tags"] = split_csv(args.tags)
	if args.category:
		body["category"] = args.category
	if args.archive:
		body["archive"] = True
	resp: Response = client.request("POST", "/api/import", body=body)
	ensure_ok(resp)
	if args.json:
		pjson(resp.data)
		return
	print(paint(f"imported {resp.data.get('imported')}, skipped {resp.data.get('skipped')}", "green"))


def cmd_export(client: WaypointdClient, args: argparse.Namespace) -> None:
	# GET /api/export returns the bookmark file (html/json/csv). The response
	# is raw bytes; write them to --out or stream them to stdout.
	resp: Response = client.request("GET", "/api/export", params={"format": args.format})
	ensure_ok(resp)
	if args.out:
		f: BinaryIO
		with open(args.out, "wb") as f:
			f.write(resp.raw)
	else:
		sys.stdout.buffer.write(resp.raw)


def cmd_check_run(client: WaypointdClient, args: argparse.Namespace) -> None:
	# POST /api/check kicks off an async liveness sweep; --delete/--hard-delete
	# decide what happens to dead links, --jobs caps the worker threads.
	if args.delete and args.hard_delete:
		raise CommandError("--delete and --hard-delete are mutually exclusive")
	body: dict = {}
	if args.delete:
		body["delete"] = True
	if args.hard_delete:
		body["hardDelete"] = True
	if args.jobs:
		body["jobs"] = args.jobs
	resp: Response = client.request("POST", "/api/check", body=body)
	ensure_ok(resp)
	if args.json:
		pjson(resp.data)
		return
	job_id: Any = (resp.data or {}).get("id")
	print(paint(f"check job started: id {job_id}  (poll with: waypoint check-status {job_id})", "green"))


def cmd_check_status(client: WaypointdClient, args: argparse.Namespace) -> None:
	# GET /api/check/{id} polls a running job; render a short one-liner for
	# running/finished/failed instead of dumping the whole JSON.
	resp: Response = client.request("GET", f"/api/check/{args.id}")
	ensure_ok(resp)
	d: dict = resp.data or {}
	if args.json:
		pjson(d)
		return
	status: str | None = d.get("status")
	if status == "running":
		print(f"running: {d.get('checked')}/{d.get('total')} checked, {d.get('dead')} dead")
	elif status == "finished":
		print(
			f"finished: {d.get('checked')} checked, {d.get('alive')} alive, "
			f"{d.get('skipped')} skipped, {d.get('deleted')} deleted, "
			f"{len(d.get('dead') or [])} dead"
		)
		dead: dict
		for dead in d.get("dead") or []:
			print(f"  dead #{dead.get('id')} {dead.get('title') or dead.get('url')}: {dead.get('reason')}")
	elif status == "failed":
		print(f"failed: {d.get('error')}")
	else:
		pjson(d)


def _paged_stats(
	client: WaypointdClient,
	args: argparse.Namespace,
	path: str,
	cols: list[str],
	rowmap: Callable[[dict], list[str]],
) -> None:
	# Shared renderer for the paged stats endpoints (domains/tags/top-visited/
	# never-visited): forward --limit/--offset, print a table, default to "none".
	params: dict[str, str] = {}
	if getattr(args, "limit", None):
		params["limit"] = str(args.limit)
	if getattr(args, "offset", None) is not None:
		params["offset"] = str(args.offset)
	resp: Response = client.request("GET", path, params=params)
	ensure_ok(resp)
	if args.json:
		pjson(resp.data)
		return
	rows: list[list[str]] = [rowmap(r) for r in (resp.data or [])]
	if rows:
		print_table(cols, rows)
	else:
		print("none")


def cmd_stats(client: WaypointdClient, args: argparse.Namespace) -> None:
	# GET /api/stats: one overview object with counts plus the top categories,
	# domains, tags, and recent bookmark lists, each printed under a header.
	resp: Response = client.request("GET", "/api/stats")
	ensure_ok(resp)
	d: dict = resp.data or {}
	if args.json:
		pjson(d)
		return
	print(
		f"total: {d.get('total')}  starred: {d.get('starred')}  "
		f"archived: {d.get('archived')}  trashed: {d.get('trashed')}"
	)
	key: str
	cols: list[str]
	rowmap: Callable[[dict], list[str]]
	for key, cols, rowmap in (
		("categories", ["NAME", "COUNT"], lambda r: [r.get("name", ""), str(r.get("count", ""))]),
		("top_domains", ["DOMAIN", "COUNT"], lambda r: [r.get("domain", ""), str(r.get("count", ""))]),
		("top_tags", ["NAME", "COUNT"], lambda r: [r.get("name", ""), str(r.get("count", ""))]),
	):
		rows: list[list[str]] = [rowmap(r) for r in (d.get(key) or [])]
		if rows:
			print(paint(key.upper(), "bold"))
			print_table(cols, rows)
	for key in ("most_visited", "recently_added"):
		rows = [
			[
				str(b.get("id", "")),
				b.get("title") or "",
				b.get("url") or "",
				str(b.get("visit_count", "")),
			]
			for b in (d.get(key) or [])
		]
		if rows:
			print(paint(key.upper(), "bold"))
			print_table(["ID", "TITLE", "URL", "VISITS"], rows)


def cmd_stats_domains(client: WaypointdClient, args: argparse.Namespace) -> None:
	_paged_stats(client, args, "/api/stats/domains", ["DOMAIN", "COUNT"],
	             lambda r: [r.get("domain", ""), str(r.get("count", ""))])


def cmd_stats_tags(client: WaypointdClient, args: argparse.Namespace) -> None:
	_paged_stats(client, args, "/api/stats/tags", ["NAME", "COUNT"],
	             lambda r: [r.get("name", ""), str(r.get("count", ""))])


def cmd_stats_top_visited(client: WaypointdClient, args: argparse.Namespace) -> None:
	_paged_stats(client, args, "/api/stats/top-visited", ["DOMAIN", "VISITS", "BOOKMARKS"],
	             lambda r: [r.get("domain", ""), str(r.get("total_visits", "")), str(r.get("bookmark_count", ""))])


def cmd_stats_never_visited(client: WaypointdClient, args: argparse.Namespace) -> None:
	_paged_stats(client, args, "/api/stats/never-visited", ["ID", "TITLE", "URL"],
	             lambda r: [str(r.get("id", "")), r.get("title") or "", r.get("url") or ""])


def cmd_stats_orphan_tags(client: WaypointdClient, args: argparse.Namespace) -> None:
	_paged_stats(client, args, "/api/stats/orphan-tags", ["NAME", "BOOKMARK", "TITLE"],
	             lambda r: [r.get("name", ""), str(r.get("bookmark_id", "")), r.get("bookmark_title") or ""])


def cmd_stats_hygiene(client: WaypointdClient, args: argparse.Namespace) -> None:
	resp: Response = client.request("GET", "/api/stats/hygiene")
	ensure_ok(resp)
	if args.json:
		pjson(resp.data)
		return
	d: dict = resp.data or {}
	print(f"total: {d.get('total')}  missing tags: {d.get('missing_tags')}  "
	      f"missing note: {d.get('missing_note')}  missing description: {d.get('missing_description')}")


def cmd_stats_activity(client: WaypointdClient, args: argparse.Namespace) -> None:
	_paged_stats(client, args, "/api/stats/activity", ["MONTH", "COUNT"],
	             lambda r: [r.get("month", ""), str(r.get("count", ""))])


def cmd_stats_bookmark(client: WaypointdClient, args: argparse.Namespace) -> None:
	# stats/bookmarks/{id} returns a full Bookmark; reuse the single-bookmark
	# renderer rather than duplicating the field list.
	resp: Response = client.request("GET", f"/api/stats/bookmarks/{args.id}")
	ensure_ok(resp)
	emit_bookmark(resp.data, args)


def cmd_backup(client: WaypointdClient, args: argparse.Namespace) -> None:
	# POST /api/admin/backup snapshots the SQLite DB to a timestamped file and
	# prunes old backups; report the path + prune count.
	resp: Response = client.request("POST", "/api/admin/backup")
	ensure_ok(resp)
	d: dict = resp.data or {}
	if args.json:
		pjson(d)
		return
	print(f"backup written: {d.get('path')}  (pruned {d.get('pruned')}, created {d.get('created_at')})")


def cmd_signin(client: WaypointdClient, args: argparse.Namespace) -> None:
	# POST /api/auth/signin exchanges the shared token for a session cookie
	# that the client will send on subsequent requests.
	resp: Response = client.request("POST", "/api/auth/signin", body={"token": args.token})
	ensure_ok(resp)
	d: dict = resp.data or {}
	if args.json:
		pjson(d)
		return
	print(f"auth enabled: {d.get('auth_enabled')}  authenticated: {d.get('authenticated')}  "
	      f"read-only: {d.get('read_only')}")


def cmd_signout(client: WaypointdClient, args: argparse.Namespace) -> None:
	# POST /api/auth/signout clears the session cookie on the server side.
	resp: Response = client.request("POST", "/api/auth/signout")
	ensure_ok(resp)
	print(paint("signed out", "green"))


def cmd_auth_status(client: WaypointdClient, args: argparse.Namespace) -> None:
	# GET /api/auth/status reflects the current session; used to check whether
	# an auth token is actually required.
	resp: Response = client.request("GET", "/api/auth/status")
	ensure_ok(resp)
	d: dict = resp.data or {}
	if args.json:
		pjson(d)
		return
	print(f"auth enabled: {d.get('auth_enabled')}  authenticated: {d.get('authenticated')}  "
	      f"read-only: {d.get('read_only')}")


def cmd_open(client: WaypointdClient, args: argparse.Namespace) -> None:
	# GET /open/{id} redirects to the bookmark's URL (307 + Location); print
	# the target so the CLI can be piped into a browser or curl.
	resp: Response = client.request("GET", f"/open/{args.id}")
	loc: str | None = resp.headers.get("location")
	if resp.status == 307 and loc:
		print(loc)
		return
	raise CommandError(
		resp.error_message() or f"expected a 307 redirect, got {resp.status}",
		code=resp.error_code(),
		status=resp.status,
	)


def _raw_text(client: WaypointdClient, path: str) -> None:
	# Used by health/ready/metrics/keywords: these endpoints return plain
	# text, so write the raw bytes straight to stdout.
	resp: Response = client.request("GET", path)
	ensure_ok(resp)
	sys.stdout.buffer.write(resp.raw)


def cmd_health(client, args):
	# /healthz is plain text ("ok"); pipe it or just eyeball the exit code.
	_raw_text(client, "/healthz")


def cmd_ready(client, args):
	_raw_text(client, "/readyz")


def cmd_metrics(client, args):
	# Prometheus text format; usually piped straight to a collector.
	_raw_text(client, "/metrics")


def cmd_keywords(client, args):
	# Plain-text keyword shortcut map: `keyword<TAB>url` lines.
	_raw_text(client, "/keywords")


# ---------------------------------------------------------------------------
# Parser
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
	# The root parser holds only the global options; every command is a
	# subparser so `--json` (and per-command options) can differ per command.
	p: argparse.ArgumentParser = argparse.ArgumentParser(
		prog="waypoint",
		description="CLI interface for waypointd server",
		epilog="Exit codes: 0 success, 1 server/validation error, 2 connection error.",
	)
	# Global options: -u/--url, -t/--token, -J/--json, -T/--timeout.
	p.add_argument("-u", "--url", dest="base_url", help=f"server base URL (default: $WAYPOINTD_SERVE_HOST + $WAYPOINTD_SERVE_PORT, else {DEFAULT_URL})")
	p.add_argument("-t", "--token", dest="bearer", help="bearer token for /api/* (default: $WAYPOINTD_TOKEN, then WAYPOINTD_SERVE_TOKEN)")
	p.add_argument("-J", "--json", action="store_true", help="print raw JSON instead of the human-readable view")
	p.add_argument("-T", "--timeout", type=int, help="per-request timeout in seconds (default 30)")

	sub: argparse._SubParsersAction = p.add_subparsers(dest="command", required=True, metavar="COMMAND")

	def _make_sub(name: str, help: str) -> argparse.ArgumentParser:
		sp: argparse.ArgumentParser = sub.add_parser(name, help=help)
		# Every subcommand accepts -J/--json. It's declared with a SUPPRESS
		# default so it never shadows the global flag and never clutters each
		# subparser's help output (help text suppressed too).
		sp.add_argument("-J", "--json", action="store_true", dest="json", default=argparse.SUPPRESS, help=argparse.SUPPRESS)
		return sp

	# ── Bookmark listing / CRUD ───────────────────────────────────────────
	sp: argparse.ArgumentParser = _make_sub("list", help="list bookmarks (active by default)")
	add_list_filters(sp)
	sp.add_argument("-e", "--all", action="store_true", help="walk every page via the cursor")
	sp.set_defaults(func=cmd_list)

	sp = _make_sub("add", help="add a bookmark (POST /api/bookmarks)")
	sp.add_argument("url")
	add_new_fields(sp)
	sp.set_defaults(func=cmd_add)

	sp = _make_sub("get", help="show one bookmark")
	sp.add_argument("id", type=int)
	sp.set_defaults(func=cmd_get)

	sp = _make_sub("note", help="print one bookmark's note as plain text")
	sp.add_argument("id", type=int)
	sp.set_defaults(func=cmd_note)

	sp = _make_sub("update", help="update one bookmark (PUT)")
	sp.add_argument("id", type=int)
	add_update_fields(sp)
	sp.set_defaults(func=cmd_update)

	sp = _make_sub("delete", help="delete one bookmark")
	sp.add_argument("id", type=int)
	sp.add_argument("-P", "--purge", action="store_true", help="permanently delete instead of trashing")
	sp.set_defaults(func=cmd_delete)

	sp = _make_sub("restore", help="restore a bookmark from the trash")
	sp.add_argument("id", type=int)
	sp.set_defaults(func=cmd_restore)

	sp = _make_sub("check", help="liveness check for one bookmark")
	sp.add_argument("id", type=int)
	sp.set_defaults(func=cmd_check_one)

	# ── Bulk operations ───────────────────────────────────────────────────
	sp = _make_sub("bulk-update", help="PATCH /api/bookmarks in bulk")
	sp.add_argument("-i", "--ids", required=True, metavar="1,2,3")
	add_update_fields(sp)
	sp.set_defaults(func=cmd_bulk_update)

	sp = _make_sub("bulk-delete", help="bulk delete by ids or filters")
	sp.add_argument("-i", "--ids", metavar="1,2,3")
	add_list_filters(sp, paging=False, cursor=False)
	sp.add_argument("-n", "--dry-run", action="store_true", help="preview without removing")
	sp.set_defaults(func=cmd_bulk_delete)

	sp = _make_sub("empty-trash", help="purge trashed bookmarks")
	sp.add_argument("-b", "--before")
	sp.add_argument("-n", "--dry-run", action="store_true")
	sp.set_defaults(func=cmd_empty_trash)

	# ── Categories and tags ───────────────────────────────────────────────
	sp = _make_sub("categories", help="list categories")
	sp.set_defaults(func=cmd_categories)
	sp = _make_sub("category-rename", help="rename a category")
	sp.add_argument("id", type=int)
	sp.add_argument("name")
	sp.set_defaults(func=cmd_category_rename)
	sp = _make_sub("category-delete", help="delete a category")
	sp.add_argument("id", type=int)
	sp.set_defaults(func=cmd_category_delete)

	sp = _make_sub("tags", help="list tags")
	sp.set_defaults(func=cmd_tags)
	sp = _make_sub("tag-rename", help="rename a tag")
	sp.add_argument("name")
	sp.add_argument("new_name", metavar="NEW_NAME")
	sp.set_defaults(func=cmd_tag_rename)
	sp = _make_sub("tag-delete", help="delete a tag")
	sp.add_argument("name")
	sp.set_defaults(func=cmd_tag_delete)

	# ── Search, import, export ────────────────────────────────────────────
	sp = _make_sub("search", help="full-text search")
	sp.add_argument("q")
	sp.add_argument("-c", "--category")
	sp.add_argument("-g", "--tag")
	sp.add_argument("-k", "--keyword")
	sp.add_argument("-l", "--limit", type=int)
	g: argparse._MutuallyExclusiveGroup = sp.add_mutually_exclusive_group()
	g.add_argument("-A", "--archived", action="store_true")
	g.add_argument("-a", "--active", action="store_true")
	sp.set_defaults(func=cmd_search)

	sp = _make_sub("import", help="import a Netscape HTML bookmark file")
	sp.add_argument("file", help="path to the file, or '-' for stdin")
	sp.add_argument("-g", "--tags", metavar="A,B")
	sp.add_argument("-c", "--category")
	sp.add_argument("-r", "--archive", action="store_true")
	sp.set_defaults(func=cmd_import)

	sp = _make_sub("export", help="export bookmarks (markdown or csv)")
	sp.add_argument("-f", "--format", choices=["md", "csv"], default="md")
	sp.add_argument("-o", "--out", help="write to a file instead of stdout")
	sp.set_defaults(func=cmd_export)

	# ── Dead-link checking ────────────────────────────────────────────────
	sp = _make_sub("check-run", help="start a batch dead-link check")
	sp.add_argument("-d", "--delete", action="store_true", help="move dead links to the trash")
	sp.add_argument("-D", "--hard-delete", action="store_true", help="purge dead links")
	sp.add_argument("-j", "--jobs", type=int, help="worker threads")
	sp.set_defaults(func=cmd_check_run)

	sp = _make_sub("check-status", help="poll a running check job")
	sp.add_argument("id", type=int)
	sp.set_defaults(func=cmd_check_status)

	# ── Statistics ────────────────────────────────────────────────────────
	sp = _make_sub("stats", help="overview statistics")
	sp.set_defaults(func=cmd_stats)
	# Paged stats commands all take the same -l/--limit and -o/--offset pair.
	sp = _make_sub("stats-domains", help="top domains")
	sp.add_argument("-l", "--limit", type=int)
	sp.add_argument("-o", "--offset", type=int)
	sp.set_defaults(func=cmd_stats_domains)
	sp = _make_sub("stats-tags", help="top tags")
	sp.add_argument("-l", "--limit", type=int)
	sp.add_argument("-o", "--offset", type=int)
	sp.set_defaults(func=cmd_stats_tags)
	sp = _make_sub("stats-top-visited", help="most visited domains")
	sp.add_argument("-l", "--limit", type=int)
	sp.add_argument("-o", "--offset", type=int)
	sp.set_defaults(func=cmd_stats_top_visited)
	sp = _make_sub("stats-never-visited", help="never-visited bookmarks")
	sp.add_argument("-l", "--limit", type=int)
	sp.add_argument("-o", "--offset", type=int)
	sp.set_defaults(func=cmd_stats_never_visited)
	sp = _make_sub("stats-orphan-tags", help="orphaned tags")
	sp.add_argument("-l", "--limit", type=int)
	sp.add_argument("-o", "--offset", type=int)
	sp.set_defaults(func=cmd_stats_orphan_tags)
	sp = _make_sub("stats-hygiene", help="hygiene counts")
	sp.set_defaults(func=cmd_stats_hygiene)
	sp = _make_sub("stats-activity", help="bookmarks per month")
	sp.add_argument("-l", "--limit", type=int)
	sp.add_argument("-o", "--offset", type=int)
	sp.set_defaults(func=cmd_stats_activity)
	sp = _make_sub("stats-bookmark", help="stats for one bookmark")
	sp.add_argument("id", type=int)
	sp.set_defaults(func=cmd_stats_bookmark)

	# ── Admin / auth ──────────────────────────────────────────────────────
	sp = _make_sub("backup", help="write a manual backup snapshot")
	sp.set_defaults(func=cmd_backup)

	sp = _make_sub("signin", help="validate a token against the server")
	sp.add_argument("token")
	sp.set_defaults(func=cmd_signin)
	sp = _make_sub("signout", help="clear the session cookie")
	sp.set_defaults(func=cmd_signout)
	sp = _make_sub("auth-status", help="report current auth state")
	sp.set_defaults(func=cmd_auth_status)

	# ── Public / ops endpoints ────────────────────────────────────────────
	sp = _make_sub("open", help="print where GET /open/{id} redirects")
	sp.add_argument("id", type=int)
	sp.set_defaults(func=cmd_open)

	sp = _make_sub("keywords", help="print the plain-text keyword list")
	sp.set_defaults(func=cmd_keywords)
	sp = _make_sub("health", help="GET /healthz")
	sp.set_defaults(func=cmd_health)
	sp = _make_sub("ready", help="GET /readyz")
	sp.set_defaults(func=cmd_ready)
	sp = _make_sub("metrics", help="GET /metrics (prometheus text)")
	sp.set_defaults(func=cmd_metrics)

	return p


HANDLERS: dict[str, Callable[[WaypointdClient, argparse.Namespace], None]] = {
	"list": cmd_list,
	"add": cmd_add,
	"get": cmd_get,
	"note": cmd_note,
	"update": cmd_update,
	"delete": cmd_delete,
	"restore": cmd_restore,
	"check": cmd_check_one,
	"bulk-update": cmd_bulk_update,
	"bulk-delete": cmd_bulk_delete,
	"empty-trash": cmd_empty_trash,
	"categories": cmd_categories,
	"category-rename": cmd_category_rename,
	"category-delete": cmd_category_delete,
	"tags": cmd_tags,
	"tag-rename": cmd_tag_rename,
	"tag-delete": cmd_tag_delete,
	"search": cmd_search,
	"import": cmd_import,
	"export": cmd_export,
	"check-run": cmd_check_run,
	"check-status": cmd_check_status,
	"stats": cmd_stats,
	"stats-domains": cmd_stats_domains,
	"stats-tags": cmd_stats_tags,
	"stats-top-visited": cmd_stats_top_visited,
	"stats-never-visited": cmd_stats_never_visited,
	"stats-orphan-tags": cmd_stats_orphan_tags,
	"stats-hygiene": cmd_stats_hygiene,
	"stats-activity": cmd_stats_activity,
	"stats-bookmark": cmd_stats_bookmark,
	"backup": cmd_backup,
	"signin": cmd_signin,
	"signout": cmd_signout,
	"auth-status": cmd_auth_status,
	"open": cmd_open,
	"keywords": cmd_keywords,
	"health": cmd_health,
	"ready": cmd_ready,
	"metrics": cmd_metrics,
}


def make_client(ns: argparse.Namespace) -> WaypointdClient:
	url: str = ns.base_url or default_url()
	token: str | None = (
		ns.bearer
		or os.environ.get("WAYPOINTD_TOKEN")
		or os.environ.get("WAYPOINTD_SERVE_TOKEN")
		or os.environ.get("WAYPOINTD_READ_TOKEN")
	)
	return WaypointdClient(url, token, ns.timeout or 30)


def dispatch(ns: argparse.Namespace, client: WaypointdClient) -> None:
	handler: Callable[[WaypointdClient, argparse.Namespace], None] | None = HANDLERS.get(ns.command)
	if handler is None:
		return
	handler(client, ns)


def main() -> None:
	parser: argparse.ArgumentParser = build_parser()
	args: argparse.Namespace = parser.parse_args()
	client: WaypointdClient = make_client(args)
	try:
		dispatch(args, client)
	except CommandError as e:
		e.print()
		sys.exit(1)
	except NetworkError as e:
		e.print()
		sys.exit(2)


if __name__ == "__main__":
	main()

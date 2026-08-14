#!/usr/bin/env python3

# marks2waypoint — one-shot importer for browser-exported bookmark .txt files.
#
# Each .txt file becomes a waypoint category (the filename, prettified, or the
# --category override). --append-tag merges extra tags into every bookmark.
# The script parses the files in Python, then drives every read and write
# through the waypoint HTTP API (POST/PUT /api/bookmarks) — it never touches
# the database directly and never shells out to a binary, so it works against
# any running waypointd, local or remote.
#
# Idempotent by design: run it repeatedly on the same files. URLs already on
# the server are updated when the file line supplies a differing
# title/description/tags, and reported as up to date otherwise; a field the
# line omits is left alone. The add-vs-update decision is made from a
# cursor-paginated snapshot of all active bookmarks fetched once up front.
#
# The API client and env-var handling are reused from the sibling `waypoint.py`
# CLI: WAYPOINTD_SERVE_HOST + WAYPOINTD_SERVE_PORT pick the server, and
# WAYPOINTD_TOKEN / WAYPOINTD_SERVE_TOKEN / WAYPOINTD_READ_TOKEN authenticate.

import argparse
import sys
from pathlib import Path
from typing import TextIO, TypedDict

# Make the sibling CLI importable regardless of how this script is invoked
# (./scripts/marks2waypoint.py, python scripts/marks2waypoint.py, ...).
sys.path.insert(0, str(Path(__file__).resolve().parent))
import waypoint


class ParsedBookmark(TypedDict):
	"""One bookmark parsed from a .txt line."""

	title: str
	url: str
	description: str
	tags: list[str]


class StoredBookmark(TypedDict):
	"""A bookmark's current state, read back from the server."""

	id: int
	title: str
	description: str
	category: str
	tags: list[str]


class CategoryGroup(TypedDict):
	"""The parsed bookmarks of one .txt file, under its category name."""

	category: str
	bookmarks: list[ParsedBookmark]


# Known URL schemes. The column classifier relies on these prefixes to spot
# the URL regardless of which column it lands in, instead of trusting column
# order — so "TITLE | URL | ..." and "URL | TITLE | ..." both work.
URL_SCHEMES: tuple[str, ...] = (
	"http://",
	"https://",
	"file://",
	"ftp://",
	"ftps://",
	"sftp://",
	"ws://",
	"wss://",
	"ssh://",
	"git://",
	"mailto:",
	"tel:",
	"sms:",
	"geo:",
	"magnet:",
	"gemini://",
	"gopher://",
	"data:",
	"ipfs://",
	"ipns://",
	"webcal://",
	"spotify:",
	"steam://",
	"slack://",
	"zoommtg://",
	"discord://",
)


def format_category_name(filename: str) -> str:
	"""Format filename to nice category name"""
	name: str = Path(filename).stem
	name = name.replace("_", " ")
	capitalized: list[str] = []
	word: str
	for word in name.split():
		capitalized.append(word.capitalize())
	name = " ".join(capitalized)
	return name


def parse_bookmark_line(line: str, source: str = "") -> ParsedBookmark | None:
	"""Parse a single bookmark line. Returns None for blank, comment, or
	malformed (>4 columns) lines. Format, no header row:
	    <TITLE> | <URL> | <DESCRIPTION> | <#FIRST_TAG #SECOND_TAG>"""
	if not line or line.strip().startswith("#"):
		return None

	# Split on "|" and drop empty segments, so "A | | c" and "A | c" both
	# parse and trailing pipes don't leave junk columns.
	parts: list[str] = []
	part: str
	for part in line.strip().split("|"):
		stripped: str = part.strip()
		if stripped:
			parts.append(stripped)

	# Spec: at most 4 columns (TITLE | URL | DESCRIPTION | TAGS).
	if len(parts) > 4:
		print(f"   ⚠️ {source}: {len(parts)} columns (max 4) — skipping: {line.strip()[:100]}")
		return None

	title: str = ""
	url: str = ""
	description: str = ""
	tags: list[str] = []

	# Classify each column by content rather than position: a known scheme
	# makes it the URL, a leading "#" makes it a tag group, and the first
	# leftover column is the title, the second the description.
	for part in parts:
		if part.startswith(URL_SCHEMES):
			url = part
		elif part.startswith("#"):
			# One column can hold several space-separated tags
			# ("#alpha #beta"); the "#" prefix is stripped from each.
			tag: str
			for tag in part.split():
				tag_clean: str = tag.replace("#", "").strip()
				if tag.startswith("#") and tag_clean:
					tags.append(tag_clean)
		elif not title:
			title = part
		elif not description:
			description = part

	# A line without a URL isn't a bookmark (e.g. a stray note) — drop it.
	if not url:
		return None

	return {
		"title": title,
		"url": url,
		"description": description,
		"tags": tags,
	}


def connect() -> waypoint.WaypointdClient:
	"""Client against the running server, reusing the sibling CLI's URL and
	token resolution (WAYPOINTD_SERVE_HOST / WAYPOINTD_SERVE_PORT and the
	token envs)."""
	return waypoint.make_client(
		argparse.Namespace(base_url=None, bearer=None, timeout=None)
	)


def stored_from_bookmark(b: dict) -> StoredBookmark:
	"""Map a `Bookmark` JSON object (from GET /api/bookmarks) onto the
	diff shape."""
	return {
		"id": b["id"],
		"title": b.get("title") or "",
		"description": b.get("description") or "",
		"category": b.get("category_name") or "",
		"tags": b.get("tags") or [],
	}


class Server:
	"""Thin wrapper over the waypoint HTTP API. Reads and writes only."""

	client: waypoint.WaypointdClient

	def __init__(self, client: waypoint.WaypointdClient) -> None:
		self.client = client

	def index(self) -> dict[str, StoredBookmark]:
		"""Every active bookmark, keyed by URL — the idempotency diff.
		Cursor-paginated so it stays correct past the default 200-row page."""
		index: dict[str, StoredBookmark] = {}
		params: dict[str, str] = {"limit": "200"}
		while True:
			resp: waypoint.Response = self.client.request("GET", "/api/bookmarks", params=params)
			waypoint.ensure_ok(resp)
			bookmark: dict
			for bookmark in resp.data or []:
				index[bookmark["url"]] = stored_from_bookmark(bookmark)
			next_cursor: str | None = resp.headers.get("x-next-cursor")
			if not next_cursor:
				break
			params.pop("offset", None)
			params["cursor"] = next_cursor
		return index

	def refresh_index(self, index: dict[str, StoredBookmark]) -> None:
		index.clear()
		index.update(self.index())

	def add(self, payload: dict) -> dict:
		resp: waypoint.Response = self.client.request("POST", "/api/bookmarks", body=payload)
		waypoint.ensure_ok(resp)
		return resp.data

	def update(self, bookmark_id: int, payload: dict) -> dict:
		resp: waypoint.Response = self.client.request("PUT", f"/api/bookmarks/{bookmark_id}", body=payload)
		waypoint.ensure_ok(resp)
		return resp.data

	def get(self, bookmark_id: int) -> dict:
		resp: waypoint.Response = self.client.request("GET", f"/api/bookmarks/{bookmark_id}")
		waypoint.ensure_ok(resp)
		return resp.data


def add_payload(bookmark: ParsedBookmark, category: str) -> dict:
	"""POST /api/bookmarks body. Empty fields are simply omitted so the
	server applies its defaults (title falls back to the URL)."""
	payload: dict = {"url": bookmark["url"], "category": category}
	if bookmark["title"]:
		payload["title"] = bookmark["title"]
	if bookmark["description"]:
		payload["description"] = bookmark["description"]
	if bookmark["tags"]:
		# De-dup and sort so the same bookmark always sends the same payload.
		payload["tags"] = sorted(set(bookmark["tags"]))
	return payload


def update_payload(
	bookmark: ParsedBookmark,
	existing: StoredBookmark,
	category_override: str | None,
) -> dict | None:
	"""Diff the parsed bookmark against the stored row; returns the tri-state
	PUT body (absent field = unchanged), or None when nothing changed. A field
	the file line does not supply is left alone — the file only ever touches a
	field by providing a value for it, so an omitted description or tag list
	never clears the stored one. `category_override` (None without --category)
	is the only case that moves a bookmark on update; a filename-derived
	category never is."""
	payload: dict = {}
	if bookmark["title"] and bookmark["title"] != existing["title"]:
		payload["title"] = bookmark["title"]
	if bookmark["description"] and bookmark["description"] != existing["description"]:
		payload["description"] = bookmark["description"]
	new_tags: list[str] = sorted(set(bookmark["tags"]))
	if bookmark["tags"] and new_tags != sorted(existing["tags"]):
		payload["tags"] = new_tags
	if category_override is not None and category_override != existing["category"]:
		payload["category"] = category_override
	return payload or None


def sync_one(
	server: Server,
	index: dict[str, StoredBookmark],
	bookmark: ParsedBookmark,
	category: str,
	category_override: str | None,
	allow_refresh: bool = True,
) -> tuple[str, str]:
	"""Add a new bookmark or update an existing one. Returns (action,
	detail); action is one of added|updated|unchanged|failed. `category` is
	the resolved category for adds (either the --category override or the
	filename-derived one); `category_override` (None without --category)
	also applies to updates."""
	existing: StoredBookmark | None = index.get(bookmark["url"])

	if existing is not None:
		payload: dict | None = update_payload(bookmark, existing, category_override)
		if payload is None:
			return "unchanged", ""
		try:
			server.update(existing["id"], payload)
		except waypoint.CommandError as e:
			return "failed", f"HTTP {e.status}: {e.message}"
		# Re-read the row so the in-memory index stays authoritative.
		index[bookmark["url"]] = stored_from_bookmark(server.get(existing["id"]))
		return "updated", ""

	try:
		created: dict = server.add(add_payload(bookmark, category))
	except waypoint.CommandError as e:
		# A 409 conflict_url means the URL appeared between our snapshot and
		# the add (a race). Rebuild the snapshot once, then re-diff.
		if e.code == "conflict_url" and allow_refresh:
			server.refresh_index(index)
			return sync_one(
				server, index, bookmark, category, category_override, allow_refresh=False
			)
		return "failed", f"HTTP {e.status}: {e.message}"
	index[bookmark["url"]] = stored_from_bookmark(created)
	return "added", ""


# ── Argument parsing ────────────────────────────────────────────────────────
parser: argparse.ArgumentParser = argparse.ArgumentParser(
	prog="marks2waypoint",
	description="Import bookmark .txt files into a running waypointd: adds new URLs, "
	"updates changed ones. Every read and write goes through the waypoint HTTP "
	"API (POST/PUT /api/bookmarks); point it at the server with "
	"WAYPOINTD_SERVE_HOST / WAYPOINTD_SERVE_PORT.",
)
parser.add_argument(
	"inputs", type=Path, nargs="+", help="Input .txt bookmark files (or directories of them)"
)
parser.add_argument(
	"--append-tag",
	action="append",
	metavar="TAG",
	default=[],
	help="Add TAG to every bookmark, merged with each line's own tags. "
	"Repeatable; each value may be space- or comma-separated "
	"(e.g. --append-tag \"work research\" --append-tag urgent).",
)
parser.add_argument(
	"--category",
	metavar="NAME",
	help="Use NAME as the category for all bookmarks instead of deriving "
	"one from each filename. Also applied to updated bookmarks, so "
	"re-running moves them into NAME.",
)
args: argparse.Namespace = parser.parse_args()

# Flatten the repeatable --append-tag values into one de-duplicated list,
# splitting each on spaces and commas so both forms work:
#     --append-tag "work research"   --append-tag urgent
#     --append-tag a,b,c
append_tags: list[str] = []
t: str
for t in args.append_tag:
	chunk: str
	for chunk in t.replace(",", " ").split():
		if chunk not in append_tags:
			append_tags.append(chunk)

# ── Resolve inputs: directories expand to their *.txt files ────────────────
txt_files: list[Path] = []

path: Path
for path in args.inputs:
	if path.is_dir():
		# A directory means "all .txt files in it"; glob() returns them
		# sorted already, and the final set() dedups overlapping inputs.
		txt_files.extend(sorted(path.glob("*.txt")))
	elif path.is_file() and path.suffix == ".txt":
		txt_files.append(path)
	else:
		parser.error(f"Input must be a .txt file or a directory of .txt files: {path}")

txt_files = sorted(set(txt_files))

# ── Parse phase: group each file's bookmarks under its category ─────────────
print("🚀 Starting Bookmark Generator...")
input_names: list[str] = []
p: Path
for p in args.inputs:
	input_names.append(str(p))
print(f"📂 Reading from: {', '.join(input_names)}")

book_marks: list[CategoryGroup] = []

file_path: Path
for file_path in txt_files:
	# An explicit --category beats the filename-derived name everywhere.
	category_name: str = args.category or format_category_name(file_path.name)

	f: TextIO
	with file_path.open("r", encoding="utf-8") as f:
		lines: list[str] = f.readlines()

	bookmarks: list[ParsedBookmark] = []

	# Parse every line, skipping blanks/comments/malformed rows. The category
	# name comes from the filename (or the --category override), not from
	# anything inside the file.
	line_no: int
	line: str
	parsed_bookmark: ParsedBookmark | None
	for line_no, line in enumerate(lines, start=1):
		parsed_bookmark = parse_bookmark_line(line, source=f"{file_path.name}:{line_no}")
		if not parsed_bookmark:
			continue

		# --append-tag additions merge with the line's own tags, so they
		# flow through both the add and the update diff.
		if append_tags:
			parsed_bookmark["tags"] = sorted(set(parsed_bookmark["tags"]) | set(append_tags))

		bookmarks.append(parsed_bookmark)

	if bookmarks:
		book_marks.append({"category": category_name, "bookmarks": bookmarks})
		print(f"   📄 Processing: {file_path.name} ({len(bookmarks)} bookmarks)")

# ── Report the parsed totals before touching the server ─────────────────────
print(f"\n📊 Total Categories: {len(book_marks)}")
total_bookmarks: int = 0
cat: CategoryGroup
for cat in book_marks:
	total_bookmarks += len(cat["bookmarks"])
print(f"📈 Total Bookmarks: {total_bookmarks}")

# ── Connect and snapshot the current bookmarks ──────────────────────────────
try:
	client: waypoint.WaypointdClient = connect()
	server: Server = Server(client)
	index: dict[str, StoredBookmark] = server.index()
except waypoint.NetworkError as e:
	e.print()
	raise SystemExit(2)

print(f"   🗄️  Connected — {len(index)} bookmark(s) already on the server")

# ── Sync phase: one add/update per bookmark, via the HTTP API ───────────────
added: int = 0
updated: int = 0
unchanged: int = 0
failed: int = 0

try:
	entry: CategoryGroup
	for entry in book_marks:
		category: str = entry["category"]
		category_override: str | None = args.category
		bookmark: ParsedBookmark
		for bookmark in entry["bookmarks"]:
			sync_result: tuple[str, str] = sync_one(
				server, index, bookmark, category, category_override
			)
			action: str = sync_result[0]
			detail: str = sync_result[1]
			label: str = bookmark["title"] or bookmark["url"]
			if action == "added":
				added += 1
				print(f"   ➕ Added: {label}")
			elif action == "updated":
				updated += 1
				print(f"   ✏️ Updated: {label}")
			elif action == "unchanged":
				unchanged += 1
				print(f"   ⏭️ Up to date: {label}")
			else:
				failed += 1
				print(f"   ❌ {label}: {detail}")
except waypoint.NetworkError as e:
	print()
	e.print()
	raise SystemExit(2)

# ── Summary ─────────────────────────────────────────────────────────────────
print(f"\n📊 Done: {added} added, {updated} updated, {unchanged} up to date, {failed} failed")

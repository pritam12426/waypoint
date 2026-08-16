// TS mirror of the Rust model structs. snake_case matches the wire format
// exactly — do not camelCase these fields.

export interface Bookmark {
	id: number;
	url: string;
	title: string;
	description: string | null;
	note: string | null;
	favicon: string | null;
	thumbnail: string | null;
	domain: string;
	category_id: number;
	category: string;
	tags: string[];
	keyword: string | null;
	redirect_template: string | null;
	visit_count: number;
	starred: boolean;
	is_archived: boolean;
	created_at: string;
	updated_at: string;
	last_visited_at: string | null;
	trashed_at: string | null;
}

export interface NewBookmark {
	url: string;
	title?: string;
	description?: string;
	note?: string;
	category?: string;
	tags?: string[];
	keyword?: string;
	/** URL template with a `{%s}` placeholder; typing `keyword <value>` in the
	 * browser bar redirects there instead of to `url`. */
	redirect_template?: string;
	favicon?: string;
	thumbnail?: string;
	starred?: boolean;
}

/**
 * Tri-state update semantics: a field that is `undefined` is left
 * unchanged; a field explicitly set to `""` (or `null` where noted) clears
 * it. Never drop keys you intend to clear.
 */
export interface UpdateBookmark {
	url?: string;
	title?: string;
	description?: string;
	note?: string;
	category?: string;
	tags?: string[];
	keyword?: string;
	/** Tri-state like `keyword`: `""` clears, a value sets (must contain
	 * `{%s}`), `undefined` leaves unchanged. */
	redirect_template?: string;
	favicon?: string;
	thumbnail?: string;
	starred?: boolean;
	is_archived?: boolean;
}

export interface Category {
	id: number;
	name: string;
}

export interface Tag {
	name: string;
	count: number;
}

export interface StatsOverview {
	total: number;
	starred: number;
	archived: number;
	trashed: number;
	categories: number;
	top_domains: { domain: string; count: number }[];
	top_tags: { name: string; count: number }[];
	most_visited: Bookmark[];
	recently_added: Bookmark[];
}

export interface DomainStat {
	domain: string;
	total_visits: number;
	bookmark_count: number;
}

export interface TagStat {
	name: string;
	count: number;
}

export interface NeverVisited {
	id: number;
	title: string;
	url: string;
	domain: string;
	created_at: string;
	favicon: string | null;
}

export interface InactiveBookmark {
	id: number;
	title: string;
	url: string;
	domain: string | null;
	favicon: string | null;
	last_visited_at: string | null;
	updated_at: string;
}

export interface Hygiene {
	total: number;
	missing_tags: number;
	missing_note: number;
	missing_description: number;
}

export interface ActivityPoint {
	period: string;
	count: number;
}

export type ActivityGranularity = "day" | "month" | "year";

export interface CheckJob {
	id: string;
	status: "pending" | "running" | "done" | "failed";
	checked: number;
	total: number;
}

export interface AuthStatus {
	authenticated: boolean;
	read_only: boolean;
}

export interface Paginated<T> {
	items: T[];
	total: number;
	next_cursor: string | null;
}

export interface BulkDeleteResult {
	ids: number[];
	removed: number;
}

export interface BulkUpdateResult {
	updated: number;
	skipped: number;
}

/** Reserved sentinel: means "use the bundled default" media asset. */
export const MEDIA_SENTINEL = "__default__";

export type ApiErrorCode =
	| "invalid_url"
	| "invalid_keyword"
	| "invalid_limit"
	| "invalid_offset"
	| "invalid_id"
	| "invalid_name"
	| "invalid_date"
	| "invalid_payload"
	| "query_required"
	| "not_found"
	| "conflict_url"
	| "conflict_keyword"
	| "unauthorized"
	| "forbidden"
	| "busy"
	| "request_timeout"
	| "idempotency_conflict"
	| "internal_error";

export interface ApiErrorBody {
	error: string;
	code: ApiErrorCode;
}

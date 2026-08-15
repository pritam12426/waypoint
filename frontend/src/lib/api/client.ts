import { useApp } from "#/lib/state";
import type { ApiErrorBody, ApiErrorCode } from "./types";

const API_BASE = import.meta.env.VITE_API_BASE ?? "";

export class ApiError extends Error {
	code: ApiErrorCode;
	status: number;

	constructor(status: number, body: ApiErrorBody) {
		super(body.error);
		this.name = "ApiError";
		this.status = status;
		this.code = body.code;
	}
}

type UnauthorizedListener = () => void;
let onUnauthorizedListener: UnauthorizedListener | null = null;

/** Registered once, in __root.tsx: clears the token and redirects to /settings. */
export function onUnauthorized(listener: UnauthorizedListener) {
	onUnauthorizedListener = listener;
}

export interface RequestOptions {
	method?: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
	body?: unknown;
	params?: Record<string, string | number | boolean | undefined | null>;
	signal?: AbortSignal;
}

export interface ApiResponse<T> {
	data: T;
	/** Lowercase per the wire contract — do not normalize casing. */
	headers: {
		"x-total-count": number | null;
		"x-next-cursor": string | null;
	};
}

function buildUrl(path: string, params?: RequestOptions["params"]) {
	const url = new URL(API_BASE + path, window.location.origin);
	if (params) {
		for (const [key, value] of Object.entries(params)) {
			if (value === undefined || value === null || value === "") continue;
			url.searchParams.set(key, String(value));
		}
	}
	return url.pathname + url.search;
}

export async function apiRequest<T>(
	path: string,
	options: RequestOptions = {},
): Promise<ApiResponse<T>> {
	// Read synchronously — do not convert this to a hook, the client is used
	// outside of React (query functions, imperative callbacks).
	const token = useApp.getState().token;

	const headers: Record<string, string> = {};
	if (token) headers.Authorization = `Bearer ${token}`;
	if (options.body !== undefined) headers["Content-Type"] = "application/json";

	const res = await fetch(buildUrl(path, options.params), {
		method: options.method ?? "GET",
		headers,
		body: options.body !== undefined ? JSON.stringify(options.body) : undefined,
		signal: options.signal,
	});

	if (res.status === 401) {
		onUnauthorizedListener?.();
	}

	if (!res.ok) {
		let body: ApiErrorBody;
		try {
			body = await res.json();
		} catch {
			body = { error: res.statusText, code: "internal_error" };
		}
		throw new ApiError(res.status, body);
	}

	const totalCount = res.headers.get("x-total-count");
	const nextCursor = res.headers.get("x-next-cursor");

	let data: T;
	if (res.status === 204) {
		data = undefined as T;
	} else {
		data = (await res.json()) as T;
	}

	return {
		data,
		headers: {
			"x-total-count": totalCount ? Number(totalCount) : null,
			"x-next-cursor": nextCursor,
		},
	};
}

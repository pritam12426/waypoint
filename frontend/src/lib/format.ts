import { format, formatDistanceToNow, parseISO } from "date-fns";

/**
 * Wire timestamps are fixed-width UTC strings with no zone suffix, e.g.
 * "2026-08-08 10:14:01". Append "Z" before parsing so date-fns treats them
 * as UTC instead of local time.
 */
export function toDate(wire: string): Date {
	const iso = wire.includes("T") ? wire : wire.replace(" ", "T");
	return parseISO(iso.endsWith("Z") ? iso : `${iso}Z`);
}

export function formatDateTime(wire: string): string {
	return format(toDate(wire), "PPp");
}

export function formatDate(wire: string): string {
	return format(toDate(wire), "PPP");
}

export function formatRelative(wire: string): string {
	return formatDistanceToNow(toDate(wire), { addSuffix: true });
}

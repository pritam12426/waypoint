import { Globe, ImageOff } from "lucide-react";
import { MEDIA_SENTINEL } from "#/lib/api/types";
import { cn } from "#/lib/utils";

function letterFor(domain: string) {
	return (
		domain
			.replace(/^www\./, "")
			.charAt(0)
			.toUpperCase() || "?"
	);
}

export interface FaviconProps {
	src?: string | null;
	domain: string;
	className?: string;
}

/** Sentinel value -> bundled default (/favicon.svg-ish letter avatar);
 * any other string -> <img>; absent -> letter avatar from the domain. */
export function Favicon({ src, domain, className }: FaviconProps) {
	if (src && src !== MEDIA_SENTINEL) {
		return (
			<img
				src={src}
				alt=""
				className={cn("size-4 rounded-sm object-contain", className)}
				loading="lazy"
				onError={(e) => {
					e.currentTarget.style.display = "none";
				}}
			/>
		);
	}
	return (
		<span
			className={cn(
				"flex size-4 items-center justify-center rounded-sm bg-muted text-[9px] font-semibold text-muted-foreground",
				className,
			)}
			aria-hidden
		>
			{src === MEDIA_SENTINEL ? <Globe className="size-3" /> : letterFor(domain)}
		</span>
	);
}

export interface ThumbnailProps {
	src?: string | null;
	domain: string;
	className?: string;
}

export function Thumbnail({ src, domain, className }: ThumbnailProps) {
	if (src && src !== MEDIA_SENTINEL) {
		return (
			<img
				src={src}
				alt=""
				className={cn(
					"aspect-video w-full rounded-md border border-border object-cover",
					className,
				)}
				loading="lazy"
			/>
		);
	}
	return (
		<div
			className={cn(
				"flex aspect-video w-full items-center justify-center rounded-md border border-dashed border-border bg-muted",
				className,
			)}
		>
			{src === MEDIA_SENTINEL ? (
				<span className="text-2xl font-semibold text-muted-foreground">
					{letterFor(domain)}
				</span>
			) : (
				<ImageOff className="size-6 text-muted-foreground" />
			)}
		</div>
	);
}

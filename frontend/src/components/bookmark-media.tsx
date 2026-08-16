import { Globe, ImageOff } from "lucide-react";
import { useState } from "react";
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

function googleFavicon(domain: string) {
	return `https://www.google.com/s2/favicons?sz=256&domain=${encodeURIComponent(domain)}`;
}

export interface FaviconProps {
	src?: string | null;
	domain: string;
	className?: string;
}

/** Google's favicon service for a domain, falling back to a letter avatar
 * if the image can't load. */
function GoogleFavicon({ domain, className }: { domain: string; className?: string }) {
	const [failed, setFailed] = useState(false);
	if (failed) {
		return (
			<span
				className={cn(
					"flex size-4 items-center justify-center rounded-sm bg-muted text-[9px] font-semibold text-muted-foreground",
					className,
				)}
				aria-hidden
			>
				{letterFor(domain)}
			</span>
		);
	}
	return (
		<img
			src={googleFavicon(domain)}
			alt=""
			className={cn("size-4 rounded-sm object-contain", className)}
			loading="lazy"
			onError={() => setFailed(true)}
		/>
	);
}

/** Sentinel value -> Globe glyph; any other string -> <img>; absent ->
 * Google's favicon service for the domain, letter avatar if that fails. */
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
	if (src !== MEDIA_SENTINEL && domain) {
		return <GoogleFavicon domain={domain} className={className} />;
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

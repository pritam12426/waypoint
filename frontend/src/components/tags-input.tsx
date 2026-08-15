import { X } from "lucide-react";
import { type KeyboardEvent, useState } from "react";
import { cn } from "#/lib/utils";

export interface TagsInputProps {
	value: string[];
	onChange: (tags: string[]) => void;
	placeholder?: string;
	className?: string;
}

/** Enter/comma adds, Backspace on empty removes last, blur commits.
 * Values are lowercased and deduped. */
export function TagsInput({
	value,
	onChange,
	placeholder = "Add a tag…",
	className,
}: TagsInputProps) {
	const [draft, setDraft] = useState("");

	function commit(raw: string) {
		const tag = raw.trim().toLowerCase();
		if (!tag) return;
		if (!value.includes(tag)) onChange([...value, tag]);
		setDraft("");
	}

	function handleKeyDown(e: KeyboardEvent<HTMLInputElement>) {
		if (e.key === "Enter" || e.key === ",") {
			e.preventDefault();
			commit(draft);
			return;
		}
		if (e.key === "Backspace" && draft === "" && value.length > 0) {
			onChange(value.slice(0, -1));
		}
	}

	function removeTag(tag: string) {
		onChange(value.filter((t) => t !== tag));
	}

	return (
		<div
			className={cn(
				"flex min-h-9 flex-wrap items-center gap-1.5 rounded-md border border-input bg-transparent px-2 py-1.5 focus-within:ring-1 focus-within:ring-ring",
				className,
			)}
		>
			{value.map((tag) => (
				<span
					key={tag}
					className="inline-flex items-center gap-1 rounded-md bg-secondary px-2 py-0.5 text-xs text-secondary-foreground"
				>
					{tag}
					<button
						type="button"
						onClick={() => removeTag(tag)}
						className="text-muted-foreground hover:text-foreground"
						aria-label={`Remove ${tag}`}
					>
						<X className="size-3" />
					</button>
				</span>
			))}
			<input
				value={draft}
				onChange={(e) => setDraft(e.target.value)}
				onKeyDown={handleKeyDown}
				onBlur={() => commit(draft)}
				placeholder={value.length === 0 ? placeholder : undefined}
				className="min-w-24 flex-1 border-none bg-transparent text-sm outline-none placeholder:text-muted-foreground"
			/>
		</div>
	);
}

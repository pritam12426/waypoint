import { zodResolver } from "@hookform/resolvers/zod";
import { useState } from "react";
import { Controller, type UseFormRegisterReturn, useForm } from "react-hook-form";
import { z } from "zod";
import { TagsInput } from "#/components/tags-input";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Input } from "#/components/ui/input";
import { Label } from "#/components/ui/label";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "#/components/ui/select";
import { Textarea } from "#/components/ui/textarea";
import type { Category, NewBookmark, UpdateBookmark } from "#/lib/api/types";
import { MEDIA_SENTINEL } from "#/lib/api/types";

const mediaModeSchema = z.enum(["auto", "default", "fetch"]);

const bookmarkFormSchema = z
	.object({
		url: z.string().url("Enter a valid URL"),
		title: z.string(),
		category: z.string(),
		description: z.string(),
		tags: z.array(z.string()),
		keyword: z.string().transform((v) => v.trim().toLowerCase()),
		// Mirrors the backend rule: a template must carry a `{%s}` placeholder
		// unless empty (an empty value clears the template).
		redirectTemplate: z.string().refine((v) => v === "" || v.includes("{%s}"), {
			message: "A redirect template must contain {%s}",
		}),
		note: z.string(),
		faviconUrl: z.string(),
		faviconMode: mediaModeSchema,
		thumbnailUrl: z.string(),
		thumbnailMode: mediaModeSchema,
		starred: z.boolean(),
	})
	.superRefine((v, ctx) => {
		// Mirrors the backend rule: a redirect template is only reachable via
		// the `/keywords/{keyword}` shortcut, so a template without a keyword
		// would never fire.
		if (v.redirectTemplate && !v.keyword) {
			ctx.addIssue({
				code: "custom",
				path: ["keyword"],
				message: "A keyword is required to use a redirect template",
			});
		}
	});

export type BookmarkFormValues = z.infer<typeof bookmarkFormSchema>;

/** Drops empty strings so the backend fills page title / default category. */
export function toNewBookmark(values: BookmarkFormValues): NewBookmark {
	const body: NewBookmark = { url: values.url };
	if (values.title) body.title = values.title;
	if (values.category) body.category = values.category;
	if (values.description) body.description = values.description;
	if (values.tags.length) body.tags = values.tags;
	if (values.keyword) body.keyword = values.keyword;
	if (values.redirectTemplate) body.redirect_template = values.redirectTemplate;
	if (values.note) body.note = values.note;
	if (values.starred) body.starred = values.starred;

	if (values.faviconMode === "default") body.favicon = MEDIA_SENTINEL;
	else if (values.faviconMode === "fetch" && values.faviconUrl)
		body.favicon = values.faviconUrl;

	if (values.thumbnailMode === "default") body.thumbnail = MEDIA_SENTINEL;
	else if (values.thumbnailMode === "fetch" && values.thumbnailUrl)
		body.thumbnail = values.thumbnailUrl;

	return body;
}

/** undefined = unchanged, "" = clear. Always sends every editable field so
 * clearing a field client-side actually clears it server-side. */
export function toUpdateBookmark(values: BookmarkFormValues): UpdateBookmark {
	const body: UpdateBookmark = {
		url: values.url,
		title: values.title,
		category: values.category,
		description: values.description,
		tags: values.tags,
		keyword: values.keyword,
		redirect_template: values.redirectTemplate,
		note: values.note,
		starred: values.starred,
	};

	if (values.faviconMode === "default") body.favicon = MEDIA_SENTINEL;
	else if (values.faviconMode === "fetch") body.favicon = values.faviconUrl;
	else body.favicon = "";

	if (values.thumbnailMode === "default") body.thumbnail = MEDIA_SENTINEL;
	else if (values.thumbnailMode === "fetch") body.thumbnail = values.thumbnailUrl;
	else body.thumbnail = "";

	return body;
}

export interface BookmarkFormProps {
	categories: Category[];
	defaultValues?: Partial<BookmarkFormValues>;
	submitLabel?: string;
	pending?: boolean;
	onSubmit: (values: BookmarkFormValues) => void;
	onCancel?: () => void;
}

export function BookmarkForm({
	categories,
	defaultValues,
	submitLabel = "Save bookmark",
	pending = false,
	onSubmit,
	onCancel,
}: BookmarkFormProps) {
	const [mediaOpen, setMediaOpen] = useState(false);
	const [addingCategory, setAddingCategory] = useState(false);

	const {
		register,
		control,
		handleSubmit,
		watch,
		setValue,
		formState: { errors },
	} = useForm<BookmarkFormValues>({
		resolver: zodResolver(bookmarkFormSchema),
		defaultValues: {
			url: "",
			title: "",
			category: "",
			description: "",
			tags: [],
			keyword: "",
			redirectTemplate: "",
			note: "",
			faviconUrl: "",
			faviconMode: "auto",
			thumbnailUrl: "",
			thumbnailMode: "auto",
			starred: false,
			...defaultValues,
		},
	});

	const category = watch("category");

	return (
		<form onSubmit={handleSubmit(onSubmit)} className="space-y-5">
			<div className="space-y-1.5">
				<Label htmlFor="url">URL</Label>
				<Input id="url" placeholder="https://example.com" {...register("url")} />
				{errors.url && <p className="text-xs text-destructive">{errors.url.message}</p>}
			</div>

			<div className="space-y-1.5">
				<Label htmlFor="title">Title</Label>
				<Input
					id="title"
					placeholder="Fetched automatically if left blank"
					{...register("title")}
				/>
			</div>

			<div className="space-y-1.5">
				<Label htmlFor="category">Category</Label>
				{addingCategory ? (
					<Input
						id="category"
						placeholder="New category name"
						{...register("category")}
						autoFocus
						onBlur={() => setAddingCategory(false)}
					/>
				) : (
					<Select
						value={category || undefined}
						onValueChange={(v) => {
							if (v === "__new__") {
								setAddingCategory(true);
								setValue("category", "");
							} else {
								setValue("category", v);
							}
						}}
					>
						<SelectTrigger id="category">
							<SelectValue placeholder="Uncategorized" />
						</SelectTrigger>
						<SelectContent>
							{categories.map((c) => (
								<SelectItem key={c.id} value={c.name}>
									{c.name}
								</SelectItem>
							))}
							<SelectItem value="__new__">+ New category…</SelectItem>
						</SelectContent>
					</Select>
				)}
			</div>

			<div className="space-y-1.5">
				<Label htmlFor="description">Description</Label>
				<Textarea id="description" rows={2} {...register("description")} />
			</div>

			<div className="space-y-1.5">
				<Label>Tags</Label>
				<Controller
					control={control}
					name="tags"
					render={({ field }) => (
						<TagsInput value={field.value} onChange={field.onChange} />
					)}
				/>
			</div>

			<div className="space-y-1.5">
				<Label htmlFor="keyword">Keyword</Label>
				<Input id="keyword" placeholder="e.g. gh" {...register("keyword")} />
				<p className="text-xs text-muted-foreground">
					Type this into your browser bar to jump here.
				</p>
			</div>

			<div className="space-y-1.5">
				<Label htmlFor="redirectTemplate">Redirect template</Label>
				<Input
					id="redirectTemplate"
					placeholder="https://example.com/search?q={%s}"
					{...register("redirectTemplate")}
				/>
				{errors.redirectTemplate && (
					<p className="text-xs text-destructive">{errors.redirectTemplate.message}</p>
				)}
				<p className="text-xs text-muted-foreground">
					Type <code>keyword value</code> in your browser bar to fill this template's{" "}
					<code>{"{%s}"}</code>. Leave blank to always jump to the URL.
				</p>
			</div>

			<div className="space-y-1.5">
				<Label htmlFor="note">Note</Label>
				<Textarea id="note" rows={4} {...register("note")} />
			</div>

			<div className="flex items-center gap-2">
				<Controller
					control={control}
					name="starred"
					render={({ field }) => (
						<Checkbox
							id="starred"
							checked={field.value}
							onCheckedChange={(v) => field.onChange(!!v)}
						/>
					)}
				/>
				<Label htmlFor="starred" className="font-normal">
					Starred
				</Label>
			</div>

			<div className="rounded-lg border border-border">
				<button
					type="button"
					onClick={() => setMediaOpen((v) => !v)}
					className="flex w-full items-center justify-between px-4 py-3 text-sm font-medium"
				>
					Media
					<span className="text-muted-foreground">{mediaOpen ? "Hide" : "Show"}</span>
				</button>
				{mediaOpen && (
					<div className="space-y-4 border-t border-border p-4">
						<MediaModeField
							label="Favicon"
							modeField={register("faviconMode")}
							urlField={register("faviconUrl")}
							mode={watch("faviconMode")}
						/>
						<MediaModeField
							label="Thumbnail"
							modeField={register("thumbnailMode")}
							urlField={register("thumbnailUrl")}
							mode={watch("thumbnailMode")}
						/>
					</div>
				)}
			</div>

			<div className="flex justify-end gap-2 pt-2">
				{onCancel && (
					<Button type="button" variant="outline" onClick={onCancel}>
						Cancel
					</Button>
				)}
				<Button type="submit" disabled={pending}>
					{submitLabel}
				</Button>
			</div>
		</form>
	);
}

function MediaModeField({
	label,
	modeField,
	urlField,
	mode,
}: {
	label: string;
	modeField: UseFormRegisterReturn;
	urlField: UseFormRegisterReturn;
	mode: "auto" | "default" | "fetch";
}) {
	return (
		<div className="space-y-1.5">
			<Label>{label}</Label>
			<div className="flex gap-3 text-sm">
				{(["auto", "default", "fetch"] as const).map((m) => (
					<label key={m} className="flex items-center gap-1.5">
						<input type="radio" value={m} {...modeField} defaultChecked={mode === m} />
						{m === "auto" ? "Auto-fetch" : m === "default" ? "Use default" : "Custom URL"}
					</label>
				))}
			</div>
			{mode === "fetch" && <Input placeholder={`${label} URL`} {...urlField} />}
		</div>
	);
}

use std::sync::Once;

use waypoint::cmd::{self, Command, bookmarks, stats};
use waypoint::logging::{LogFormat, LogLevel};

static SILENCE: Once = Once::new();

fn silence_logs() {
	SILENCE.call_once(|| {
		waypoint::logging::log_init(None, LogLevel::Off, LogFormat::Pretty);
	});
}

fn temp_db() -> (tempfile::TempDir, std::path::PathBuf) {
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("waypoint.db");
	(dir, path)
}

fn run(path: &std::path::Path, command: Command) {
	cmd::run_command(path, command).expect("command should succeed");
}

#[test]
fn add_list_get_roundtrip() {
	silence_logs();
	let (_dir, path) = temp_db();

	run(
		&path,
		Command::Bookmarks(Box::new(bookmarks::Command::Add {
			url: "https://example.com/article".to_string(),
			title: None,
			keyword: None,
			category: None,
			tags: None,
			description: None,
			note: None,
			favicon: None,
			thumbnail: None,
			no_custom_favicon: false,
			no_thumbnail: false,
			mode: None,
			starred: false,
		})),
	);

	run(
		&path,
		Command::Bookmarks(Box::new(bookmarks::Command::List(bookmarks::ListArgs {
			category: None,
			category_id: None,
			tag: None,
			keyword: None,
			starred: false,
			created_after: None,
			created_before: None,
			updated_after: None,
			updated_before: None,
			visited_after: None,
			visited_before: None,
			archived: false,
			all: false,
			limit: 50,
			json: false,
			links: false,
		}))),
	);

	run(
		&path,
		Command::Bookmarks(Box::new(bookmarks::Command::Get { id: 1, json: false })),
	);

	run(
		&path,
		Command::Stats {
			command: Some(stats::Command::Overview { json: false }),
		},
	);
}

#[test]
fn update_and_trash_restore() {
	silence_logs();
	let (_dir, path) = temp_db();

	run(
		&path,
		Command::Bookmarks(Box::new(bookmarks::Command::Add {
			url: "https://rust-lang.org".to_string(),
			title: None,
			keyword: None,
			category: None,
			tags: None,
			description: None,
			note: None,
			favicon: None,
			thumbnail: None,
			no_custom_favicon: false,
			no_thumbnail: false,
			mode: None,
			starred: false,
		})),
	);

	run(
		&path,
		Command::Bookmarks(Box::new(bookmarks::Command::Update(
			bookmarks::UpdateArgs {
				ids: vec![1],
				title: Some("Rust".to_string()),
				url: None,
				keyword: Some("rs".to_string()),
				clear_keyword: false,
				category: None,
				tags: None,
				add_tags: None,
				remove_tags: None,
				description: None,
				note: None,
				favicon: None,
				thumbnail: None,
				no_custom_favicon: false,
				no_thumbnail: false,
				mode: None,
				refresh: false,
				star: false,
				unstar: false,
				archive: false,
				unarchive: false,
			},
		))),
	);

	run(
		&path,
		Command::Bookmarks(Box::new(bookmarks::Command::Remove(
			bookmarks::RemoveArgs {
				ids: vec![1],
				purge: false,
				dry_run: false,
				category: None,
				category_id: None,
				tag: None,
				keyword: None,
				created_after: None,
				created_before: None,
				updated_after: None,
				updated_before: None,
				visited_after: None,
				visited_before: None,
			},
		))),
	);

	run(&path, Command::Trash { command: None });
	run(
		&path,
		Command::Trash {
			command: Some(waypoint::cmd::trash::Command::Restore { ids: vec![1] }),
		},
	);
}

/// The `--no-custom-favicon` / `--no-thumbnail` flags: a YouTube URL gets
/// only the generic domain favicon (never a custom/site-specific one) and
/// no thumbnail; on a URL change both re-resolve against the new URL.
#[test]
fn add_and_update_with_no_custom_media() {
	silence_logs();
	let (_dir, path) = temp_db();

	run(
		&path,
		Command::Bookmarks(Box::new(bookmarks::Command::Add {
			url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string(),
			title: None,
			keyword: None,
			category: None,
			tags: None,
			description: None,
			note: None,
			favicon: None,
			thumbnail: None,
			no_custom_favicon: true,
			no_thumbnail: true,
			mode: None,
			starred: false,
		})),
	);

	let conn = waypoint::database::open(&path).unwrap();
	let b = waypoint::database::bookmarks::get(&conn, 1)
		.unwrap()
		.unwrap();
	assert_eq!(
		b.favicon.as_deref(),
		Some("https://www.youtube.com/favicon.ico")
	);
	assert_eq!(b.thumbnail, None);

	// URL change: the generic favicon follows the new domain, no thumbnail.
	run(
		&path,
		Command::Bookmarks(Box::new(bookmarks::Command::Update(
			bookmarks::UpdateArgs {
				ids: vec![1],
				title: None,
				url: Some("https://example.com/moved".to_string()),
				keyword: None,
				clear_keyword: false,
				category: None,
				tags: None,
				add_tags: None,
				remove_tags: None,
				description: None,
				note: None,
				favicon: None,
				thumbnail: None,
				no_custom_favicon: true,
				no_thumbnail: true,
				mode: None,
				refresh: false,
				star: false,
				unstar: false,
				archive: false,
				unarchive: false,
			},
		))),
	);
	let b = waypoint::database::bookmarks::get(&conn, 1)
		.unwrap()
		.unwrap();
	assert_eq!(
		b.favicon.as_deref(),
		Some("https://example.com/favicon.ico")
	);
	assert_eq!(b.thumbnail, None);
}

/// `add --mode default` stores the bundled-asset tokens; `trash empty
/// --dry-run` previews without purging, and the real run cleans up.
#[test]
fn default_mode_and_trash_empty_dry_run() {
	silence_logs();
	let (_dir, path) = temp_db();

	run(
		&path,
		Command::Bookmarks(Box::new(bookmarks::Command::Add {
			url: "https://example.com/dflt".to_string(),
			title: None,
			keyword: None,
			category: None,
			tags: None,
			description: None,
			note: None,
			favicon: None,
			thumbnail: None,
			no_custom_favicon: false,
			no_thumbnail: false,
			mode: Some(waypoint::model::AssetMode::Default),
			starred: false,
		})),
	);

	let conn = waypoint::database::open(&path).unwrap();
	let b = waypoint::database::bookmarks::get(&conn, 1)
		.unwrap()
		.unwrap();
	assert_eq!(b.favicon.as_deref(), Some(waypoint::model::DEFAULT_FAVICON));
	assert_eq!(
		b.thumbnail.as_deref(),
		Some(waypoint::model::DEFAULT_THUMBNAIL)
	);

	// Trash it, then dry-run empty: the bookmark must survive.
	run(
		&path,
		Command::Bookmarks(Box::new(bookmarks::Command::Remove(
			bookmarks::RemoveArgs {
				ids: vec![1],
				purge: false,
				dry_run: false,
				category: None,
				category_id: None,
				tag: None,
				keyword: None,
				created_after: None,
				created_before: None,
				updated_after: None,
				updated_before: None,
				visited_after: None,
				visited_before: None,
			},
		))),
	);
	run(
		&path,
		Command::Trash {
			command: Some(waypoint::cmd::trash::Command::Empty {
				before: None,
				yes: false,
				dry_run: true,
			}),
		},
	);
	let trashed = waypoint::database::bookmarks::list(
		&conn,
		&waypoint::model::BookmarkFilter {
			trash: true,
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(trashed.len(), 1, "dry run must not purge");

	// Real empty purges it.
	run(
		&path,
		Command::Trash {
			command: Some(waypoint::cmd::trash::Command::Empty {
				before: None,
				yes: true,
				dry_run: false,
			}),
		},
	);
	let trashed = waypoint::database::bookmarks::list(
		&conn,
		&waypoint::model::BookmarkFilter {
			trash: true,
			..Default::default()
		},
	)
	.unwrap();
	assert!(trashed.is_empty());
}

// `--mode` describes resolution of the *implicit* media, so combining it
// with an explicit `--favicon`/`--thumbnail`/`--no-*` sentinel is a parse
// error instead of the mode silently discarding the explicit value.
#[test]
fn mode_conflicts_with_explicit_media_args() {
	use clap::Parser;
	use waypoint::cmd::Cli;

	// add: --mode + --favicon
	let err = Cli::try_parse_from([
		"waypoint",
		"--database",
		"/tmp/x.db",
		"bookmarks",
		"add",
		"--mode",
		"fetch",
		"--favicon",
		"https://cdn.example/f.ico",
		"https://example.com",
	])
	.unwrap_err();
	assert!(err.to_string().contains("cannot be used with"), "{err}");

	// add: --mode + --no-thumbnail
	let err = Cli::try_parse_from([
		"waypoint",
		"--database",
		"/tmp/x.db",
		"bookmarks",
		"add",
		"--mode",
		"fetch",
		"--no-thumbnail",
		"https://example.com",
	])
	.unwrap_err();
	assert!(err.to_string().contains("cannot be used with"), "{err}");

	// update: --mode + --thumbnail
	let err = Cli::try_parse_from([
		"waypoint",
		"--database",
		"/tmp/x.db",
		"bookmarks",
		"update",
		"1",
		"--mode",
		"auto",
		"--thumbnail",
		"https://cdn.example/t.png",
	])
	.unwrap_err();
	assert!(err.to_string().contains("cannot be used with"), "{err}");

	// update: --mode + --no-custom-favicon
	let err = Cli::try_parse_from([
		"waypoint",
		"--database",
		"/tmp/x.db",
		"bookmarks",
		"update",
		"1",
		"--mode",
		"auto",
		"--no-custom-favicon",
	])
	.unwrap_err();
	assert!(err.to_string().contains("cannot be used with"), "{err}");
}

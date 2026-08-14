/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Category persistence: the `categories` table, its CRUD, and its
//! bookmark counts.
//!
//! Categories are the one-to-many taxonomy (each bookmark belongs to exactly
//! one). The default category is special: it's seeded on every `open()`,
//! can't be renamed or deleted, and swallows bookmarks whose category is
//! deleted (see `delete`).
//!
//! Deleting a category *never destroys bookmarks* — the raw `ON DELETE
//! CASCADE` on `bookmarks.category_id` is deliberately neutralized here by
//! reassigning the category's bookmarks to the default first.

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::model::{Category, CategoryCount, DEFAULT_CATEGORY};

/// Ensures the default "Uncategorized" category exists and returns its id.
/// Called on every `database::open()` so a deleted default is never a
/// one-off weirdness.
pub fn ensure_default(conn: &Connection) -> Result<i64> {
	get_or_create(conn, DEFAULT_CATEGORY)
}

/// Returns the id of `name` (trimmed, falling back to the default category
/// when empty), creating the row on first use. `INSERT ... ON CONFLICT DO
/// NOTHING` makes it atomic — no check-then-insert race when two code paths
/// create a category in parallel.
pub fn get_or_create(conn: &Connection, name: &str) -> Result<i64> {
	let name = if name.trim().is_empty() {
		DEFAULT_CATEGORY
	} else {
		name.trim()
	};
	conn.execute(
		"INSERT INTO categories (name) VALUES (?1) ON CONFLICT(name) DO NOTHING",
		params![name],
	)
	.context("failed to ensure category exists")?;
	let id = conn.query_row(
		"SELECT id FROM categories WHERE name = ?1",
		params![name],
		|row| row.get(0),
	)?;
	crate::log_trace!("category {name:?} resolved to id #{id}");
	Ok(id)
}

/// All categories, alphabetical.
pub fn list(conn: &Connection) -> Result<Vec<Category>> {
	let mut stmt = conn.prepare("SELECT id, name FROM categories ORDER BY name")?;
	let rows = stmt.query_map([], |row| {
		Ok(Category {
			id: row.get(0)?,
			name: row.get(1)?,
		})
	})?;
	let categories = rows.collect::<rusqlite::Result<Vec<_>>>()?;
	crate::log_trace!("listed {} categories", categories.len());
	Ok(categories)
}

/// Whether `id` is the default category — the one the API refuses to
/// rename or delete.
pub fn is_default(conn: &Connection, id: i64) -> Result<bool> {
	let default_id = ensure_default(conn)?;
	Ok(id == default_id)
}

/// Renames a category. Returns `false` when no such category exists. Blank
/// names are rejected before touching the database; guarding the default
/// category is the caller's job (`is_default`).
pub fn rename(conn: &Connection, id: i64, new_name: &str) -> Result<bool> {
	let new_name = new_name.trim();
	if new_name.is_empty() {
		anyhow::bail!("a category needs a non-empty name");
	}
	let rows = conn.execute(
		"UPDATE categories SET name = ?1 WHERE id = ?2",
		params![new_name, id],
	)?;
	let renamed = rows > 0;
	crate::log_trace!("rename category #{id} -> {new_name:?}: {renamed}");
	Ok(renamed)
}

/// Deletes a category, first moving its bookmarks to the default category.
/// Returns `false` when no such category exists. Guarding the default
/// category is the caller's job.
pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
	// Reassign this category's bookmarks to the default before deleting,
	// otherwise the `ON DELETE CASCADE` on `bookmarks.category_id` would
	// silently destroy every bookmark in the category.
	let default_id = ensure_default(conn)?;
	conn.execute(
		"UPDATE bookmarks SET category_id = ?1 WHERE category_id = ?2",
		params![default_id, id],
	)?;
	let rows = conn.execute("DELETE FROM categories WHERE id = ?1", params![id])?;
	let deleted = rows > 0;
	crate::log_trace!("delete category #{id}: {deleted} (bookmarks reassigned to #{default_id})");
	Ok(deleted)
}

/// Each category with its active-bookmark count, most-used first. Trashed
/// bookmarks are excluded from the counts.
pub fn counts(conn: &Connection) -> Result<Vec<CategoryCount>> {
	let mut stmt = conn.prepare(
		"SELECT c.name, COUNT(b.id) as cnt
         FROM categories c
         LEFT JOIN bookmarks b ON b.category_id = c.id AND b.trashed_at IS NULL
         GROUP BY c.id
         ORDER BY cnt DESC, c.name ASC",
	)?;
	let rows = stmt.query_map([], |row| {
		Ok(CategoryCount {
			name: row.get(0)?,
			count: row.get(1)?,
		})
	})?;
	let counts = rows.collect::<rusqlite::Result<Vec<_>>>()?;
	crate::log_trace!("category counts -> {} categories", counts.len());
	Ok(counts)
}

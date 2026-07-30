/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! `log_*!` macros. `#[macro_export]` places these at the crate root
//! (`$crate::log_info!`, etc), so callers use `crate::log_*!` /
//! `waypointd::log_*!` regardless of this file's module path. Each macro
//! captures the caller's source location automatically (via `__loc!()`),
//! replacing the old `show_source_location` gate.

#[doc(hidden)]
#[macro_export]
macro_rules! __function_name {
	() => {{
		fn f() {}
		fn type_name_of<T>(_: T) -> &'static str {
			std::any::type_name::<T>()
		}
		let name = type_name_of(f);
		&name[..name.len() - 3]
	}};
}

#[cfg(all(feature = "show_source_location", debug_assertions))]
#[doc(hidden)]
#[macro_export]
macro_rules! __loc {
	() => {
		Some($crate::logging::SourceLoc {
			file: file!(),
			line: line!(),
			func: $crate::__function_name!(),
		})
	};
}

#[cfg(not(all(feature = "show_source_location", debug_assertions)))]
#[doc(hidden)]
#[macro_export]
macro_rules! __loc {
	() => {
		None
	};
}

/// Log with custom newline behaviour (`false` = no newline, `true` = with
/// newline). Used internally; prefer `log_fatal!` / `log_error!` / etc.
#[macro_export]
macro_rules! log_custom {
	($level:expr, $newline:expr, $($arg:tt)*) => {{
		$crate::logging::log_record($level, $crate::__loc!(), $newline, &format!($($arg)*));
	}};
}

/// Log an error and append the last OS error (equivalent of `perror()`).
///
/// Builds the combined message and sends it through `log_record` in a
/// single call under the logger's lock, rather than writing the formatted
/// message via `log_custom!` and then the OS-error suffix via a separate
/// raw `eprintln!`. That two-write version had two bugs: it always wrote
/// the suffix to the real process stderr even when a `--log-file` was
/// configured (so the errno text never reached the log file, and the file
/// was left with a dangling line with no trailing newline), and it
/// released the logger's lock between the two writes, so a concurrent
/// caller's line could land in between them. Doing it as one write avoids
/// both.
#[macro_export]
macro_rules! log_perror {
	($($arg:tt)*) => {{
		$crate::logging::log_record(
			$crate::logging::LogLevel::Error,
			$crate::__loc!(),
			true,
			&format!("{} {}", format!($($arg)*), ::std::io::Error::last_os_error()),
		);
	}};
}

#[macro_export]
macro_rules! log_fatal {
	($($arg:tt)*) => { $crate::log_custom!($crate::logging::LogLevel::Fatal, true, $($arg)*) };
}
#[macro_export]
macro_rules! log_error {
	($($arg:tt)*) => { $crate::log_custom!($crate::logging::LogLevel::Error, true, $($arg)*) };
}
#[macro_export]
macro_rules! log_warn {
	($($arg:tt)*) => { $crate::log_custom!($crate::logging::LogLevel::Warn, true, $($arg)*) };
}
#[macro_export]
macro_rules! log_info {
	($($arg:tt)*) => { $crate::log_custom!($crate::logging::LogLevel::Info, true, $($arg)*) };
}
#[macro_export]
macro_rules! log_debug {
	($($arg:tt)*) => { $crate::log_custom!($crate::logging::LogLevel::Debug, true, $($arg)*) };
}
#[macro_export]
macro_rules! log_trace {
	($($arg:tt)*) => { $crate::log_custom!($crate::logging::LogLevel::Trace, true, $($arg)*) };
}

/// Check if messages at the given level would be emitted right now.
#[macro_export]
macro_rules! log_level_is_enabled {
	($level:expr) => {
		$crate::logging::log_get_level() >= $level
	};
}

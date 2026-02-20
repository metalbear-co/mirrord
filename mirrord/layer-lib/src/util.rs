//! Module for utility functions, where function signatures are shared and internal
//! implementation is cfg-guarded in the same file.

use std::{env, path::PathBuf};

/// Macro to safely allocate memory for Windows structures with error handling
#[cfg(windows)]
#[macro_export]
macro_rules! unsafe_alloc {
    ($type:ty, $err: expr) => {{
        let layout = std::alloc::Layout::new::<$type>();
        let ptr = unsafe { std::alloc::alloc(layout) as *mut $type };
        if ptr.is_null() { Err($err) } else { Ok(ptr) }
    }};
}

#[cfg(windows)]
use str_win::path_to_unix_path;

pub fn get_home_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let Ok(Some(home)) = env::var("USERPROFILE").map(path_to_unix_path) else {
            return None;
        };

        let home_clean = regex::escape(home.trim_end_matches('/'));
        Some(PathBuf::from(home_clean))
    }

    #[cfg(unix)]
    {
        let Ok(home) = env::var("HOME") else {
            return None;
        };

        let home_clean = regex::escape(home.trim_end_matches('/'));
        Some(PathBuf::from(home_clean))
    }
}

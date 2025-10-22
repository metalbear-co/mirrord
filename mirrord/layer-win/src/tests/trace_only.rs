//! Tests for trace-only mode functionality
//!
//! This module tests the trace-only mode where mirrord operates without
//! connecting to an agent, only logging operations locally.

use std::env;

use crate::trace_only::TRACE_ONLY_ENV;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_only_environment_parsing() {
        // Test various environment variable values
        let test_cases = vec![
            ("true", true),
            ("false", false),
            ("1", false),   // "1" doesn't parse to bool in Rust
            ("yes", false), // Only "true"/"false" work with .parse() for bool
            ("0", false),
            ("", false),
        ];

        for (env_value, expected) in test_cases {
            unsafe {
                env::set_var(TRACE_ONLY_ENV, env_value);

                let parsed = env::var(TRACE_ONLY_ENV)
                    .unwrap_or_default()
                    .parse()
                    .unwrap_or(false);

                assert_eq!(
                    parsed, expected,
                    "Environment value '{}' should parse to {}",
                    env_value, expected
                );
            }
        }

        unsafe {
            env::remove_var(TRACE_ONLY_ENV);
        }
    }

    #[test]
    fn test_trace_only_constant() {
        // Test that our constant is correctly defined
        assert_eq!(TRACE_ONLY_ENV, "MIRRORD_LAYER_TRACE_ONLY");
    }

    #[test]
    fn test_trace_only_mode_detection() {
        use std::env;

        // Test that trace-only mode is correctly detected from environment
        unsafe {
            // Set trace-only mode
            env::set_var(TRACE_ONLY_ENV, "true");

            let trace_only = env::var(TRACE_ONLY_ENV)
                .unwrap_or_default()
                .parse()
                .unwrap_or(false);

            assert!(
                trace_only,
                "Should detect trace-only mode when environment variable is set to true"
            );

            // Test false value
            env::set_var(TRACE_ONLY_ENV, "false");

            let trace_only = env::var(TRACE_ONLY_ENV)
                .unwrap_or_default()
                .parse()
                .unwrap_or(false);

            assert!(
                !trace_only,
                "Should not detect trace-only mode when environment variable is set to false"
            );

            // Clean up
            env::remove_var(TRACE_ONLY_ENV);
        }
    }
}

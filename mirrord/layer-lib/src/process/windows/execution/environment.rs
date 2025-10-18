//! Environment management for mirrord Windows process execution.
//!
//! This module provides unified environment variable handling for both layer hooks
//! and CLI execution contexts.

use std::{collections::HashMap, ptr, time::SystemTime};
use crate::error::LayerResult;
use mirrord_config::MIRRORD_LAYER_INTPROXY_ADDR;
use str_win::string_to_u16_buffer;
use winapi::{
    shared::minwindef::LPVOID,
    um::winbase::CREATE_UNICODE_ENVIRONMENT,
};

/// Create a complete environment for process execution.
/// 
/// This function sets up the environment for Windows process execution by:
/// - Starting with current environment variables
/// - Adding mirrord-specific variables
/// - Adding layer configuration when available
/// - Preserving existing mirrord environment variables
/// - Generating initialization event name for synchronization
pub fn create_process_environment() -> LayerResult<(HashMap<String, String>, Option<String>)> {
    let mut environment: HashMap<String, String> = std::env::vars().collect();
    
    // Add common mirrord variables
    add_common_mirrord_vars(&mut environment)?;
    
    // Add layer config if available (gracefully handles when not initialized)
    add_layer_config(&mut environment)?;
    
    // Preserve existing mirrord environment variables
    preserve_mirrord_env_vars(&mut environment);
    
    // Generate init event for child synchronization
    let init_event_name = Some(generate_init_event_name());
    
    if let Some(ref event_name) = init_event_name {
        environment.insert("MIRRORD_INIT_EVENT".to_string(), event_name.clone());
    }
    
    Ok((environment, init_event_name))
}

/// Add common mirrord environment variables for both contexts.
fn add_common_mirrord_vars(environment: &mut HashMap<String, String>) -> LayerResult<()> {
    // Add proxy address from global state if available (same as Unix layer)
    if let Some(proxy_connection) = crate::proxy_connection::PROXY_CONNECTION.get() {
        environment.insert(
            MIRRORD_LAYER_INTPROXY_ADDR.to_string(),
            proxy_connection.proxy_addr().to_string(),
        );
    }

    Ok(())
}

/// Add layer config when available.
/// This gracefully handles cases where layer setup is not initialized (e.g., in tests).
fn add_layer_config(environment: &mut HashMap<String, String>) -> LayerResult<()> {
    if let Ok(config_base64) = crate::setup::layer_setup().layer_config().encode() {
        // Use the same config environment variable as all other platforms
        environment.insert(
            mirrord_config::LayerConfig::RESOLVED_CONFIG_ENV.to_string(),
            config_base64,
        );
    }
    // In tests or when not initialized, silently continue without config
    Ok(())
}

/// Preserve important mirrord environment variables.
/// This ensures variables like MIRRORD_RESOLVED_CONFIG are passed to child processes.
fn preserve_mirrord_env_vars(environment: &mut HashMap<String, String>) {
    // Preserve all mirrord environment variables from the current process
    for (key, value) in std::env::vars() {
        if key.starts_with("MIRRORD_") {
            environment.insert(key, value);
        }
    }
}

/// Generate a unique initialization event name.
fn generate_init_event_name() -> String {
    format!(
        "mirrord_init_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    )
}

/// Convert environment variables to Windows format for process creation.
///
/// This function handles the Windows-specific environment block conversion:
/// - Converts HashMap to null-terminated wide strings
/// - Returns the environment pointer and creation flags
/// - Properly handles empty environments (returns null pointer)
pub fn setup_windows_environment(
    environment: &HashMap<String, String>,
    base_creation_flags: u32,
) -> (LPVOID, u32, Vec<u16>) {
    
    let mut actual_creation_flags = base_creation_flags;
    let mut windows_environment: Vec<u16> = Vec::new();
    
    if !environment.is_empty() {
        actual_creation_flags |= CREATE_UNICODE_ENVIRONMENT;
        
        // Convert environment to Windows format
        for (key, value) in environment {
            let env_entry = format!("{}={}", key, value);
            let entry_wide = string_to_u16_buffer(&env_entry);
            windows_environment.extend(entry_wide);
        }
        // Final null terminator
        windows_environment.push(0);
        
        let environment_ptr = windows_environment.as_mut_ptr() as LPVOID;
        (environment_ptr, actual_creation_flags, windows_environment)
    } else {
        (ptr::null_mut(), actual_creation_flags, windows_environment)
    }
}

/// Creates a complete mirrord environment for layer hooks.
pub fn create_mirrord_environment() -> LayerResult<(HashMap<String, String>, Option<String>)> {
    create_process_environment()
}

/// Creates a basic mirrord environment for CLI execution.
pub fn create_cli_environment() -> LayerResult<(HashMap<String, String>, Option<String>)> {
    create_process_environment()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_process_environment() {
        let (env, event_name) = create_process_environment().unwrap();
        assert!(!env.is_empty());
        assert!(event_name.is_some()); // Always creates init events for child synchronization
    }

    #[test]
    fn test_create_environments() {
        // Test CLI environment
        let (cli_env, cli_event_name) = create_cli_environment().unwrap();
        assert!(!cli_env.is_empty());
        assert!(cli_event_name.is_some());

        // Test layer hook environment  
        let (hook_env, event_name) = create_mirrord_environment().unwrap();
        assert!(!hook_env.is_empty());
        assert!(event_name.is_some());
    }
}
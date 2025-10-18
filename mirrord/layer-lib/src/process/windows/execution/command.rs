use std::{collections::HashMap, ffi::OsStr, path::Path};
use crate::error::LayerResult;
use super::{WindowsProcessExecutor, WindowsProcessResult, MIRRORD_LAYER_FILE_ENV};
use super::environment::create_process_environment;

/// Resolves a DLL path from environment variable, converting any path to absolute canonical path.
/// This ensures that DLL injection works correctly regardless of whether the path is relative or absolute.
fn resolve_dll_path() -> LayerResult<String> {
    let dll_path = std::env::var(MIRRORD_LAYER_FILE_ENV)
        .map_err(|e| crate::error::LayerError::VarError(e))?;
    
    let path = Path::new(&dll_path);
    if !path.exists() {
        return Err(crate::error::LayerError::DllInjection(format!(
            "DLL path does not exist: {}",
            dll_path
        )));
    }
    
    Ok(dll_path)
}

/// Simple process executor for Windows with mirrord layer injection.
/// Auto-configures everything from global state and environment variables.
pub struct ProcessExecutor {
    program: String,
    args: Vec<String>,
    environment: HashMap<String, String>,
    current_dir: Option<String>,
    init_event_name: Option<String>,
}



impl ProcessExecutor {
    /// Create a new process executor for CLI context.
    pub fn new_cli<S: AsRef<OsStr>>(program: S) -> LayerResult<Self> {
        let (environment, init_event_name) = create_process_environment()?;
        
        Ok(Self {
            program: program.as_ref().to_string_lossy().to_string(),
            args: Vec::new(),
            environment,
            current_dir: None,
            init_event_name,
        })
    }

    /// Create a new process executor for layer hook context.
    pub fn new_layer_hook<S: AsRef<OsStr>>(program: S) -> LayerResult<Self> {
        let (environment, init_event_name) = create_process_environment()?;
        
        Ok(Self {
            program: program.as_ref().to_string_lossy().to_string(),
            args: Vec::new(),
            environment,
            current_dir: None,
            init_event_name,
        })
    }



    /// Add an argument.
    pub fn arg<S: AsRef<str>>(mut self, arg: S) -> Self {
        self.args.push(arg.as_ref().to_string());
        self
    }

    /// Add arguments.
    pub fn args<I, S>(mut self, args: I) -> Self 
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for arg in args {
            self.args.push(arg.as_ref().to_string());
        }
        self
    }

    /// Set environment variable.
    pub fn env<K: AsRef<str>, V: AsRef<str>>(mut self, key: K, val: V) -> Self {
        self.environment.insert(key.as_ref().to_string(), val.as_ref().to_string());
        self
    }

    /// Set multiple environment variables.
    pub fn envs<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        for (key, val) in vars {
            self.environment.insert(key.as_ref().to_string(), val.as_ref().to_string());
        }
        self
    }

    /// Set current directory.
    pub fn current_dir<P: AsRef<str>>(mut self, dir: P) -> Self {
        self.current_dir = Some(dir.as_ref().to_string());
        self
    }

    /// Execute the process with layer injection.
    pub fn spawn(self) -> LayerResult<WindowsProcessResult> {
        // Build command line - when using application_name with CreateProcessW,
        // the command line should contain only arguments, not the program name
        let command_line = if self.args.is_empty() {
            String::new()  // Empty command line when no args and application_name is provided
        } else {
            self.args.join(" ")  // Only arguments, not the program name
        };

        // Auto-configure from global state
        let dll_path = resolve_dll_path()?;
        
        let (proxy_addr, layer_id) = if let Some(proxy_connection) = crate::proxy_connection::PROXY_CONNECTION.get() {
            (proxy_connection.proxy_addr().to_string(), proxy_connection.layer_id().0 as u32)
        } else {
            ("127.0.0.1:0".to_string(), 0)
        };

        // Create WindowsProcessConfig for the unified executor
        let config = super::WindowsProcessConfig {
            application_name: Some(self.program),
            command_line,
            current_directory: self.current_dir,
            inherit_handles: false,
            creation_flags: 0,
            dll_path,
            proxy_addr,
            layer_id,
            environment: self.environment,
            original_create_process_fn: None,
        };

        let executor = WindowsProcessExecutor::new(config);
        executor.execute()
    }



    /// Get the initialization event name.
    pub fn init_event_name(&self) -> Option<&str> {
        self.init_event_name.as_deref()
    }
}

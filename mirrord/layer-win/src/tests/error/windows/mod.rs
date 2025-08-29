//! Please refer to https://learn.microsoft.com/en-us/windows/win32/debug/system-error-codes--0-499-

use winapi::shared::winerror::{ERROR_FILE_NOT_FOUND, ERROR_INVALID_FUNCTION, ERROR_SUCCESS};

use crate::error::windows::WindowsError;

#[test]
fn error_success() {
    let error = WindowsError::new(ERROR_SUCCESS);
    assert_eq!(error.get_formatted_error(), Some("The operation completed successfully.".into()));
}

#[test]
fn error_invalid_function() {
    let error = WindowsError::new(ERROR_INVALID_FUNCTION);
    assert_eq!(error.get_formatted_error(), Some("Incorrect function.".into()));
}

#[test]
fn error_file_not_found() {
    let error = WindowsError::new(ERROR_FILE_NOT_FOUND);
    assert_eq!(error.get_formatted_error(), Some("The system cannot find the file specified.".into()));
}

#[test]
fn error_bogus()
{
    let error = WindowsError::new(13370);
    assert_eq!(error.get_formatted_error(), None);
}
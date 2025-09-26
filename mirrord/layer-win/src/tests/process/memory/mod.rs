use crate::process::memory::is_memory_valid;

#[test]
fn check_null() {
    assert_eq!(is_memory_valid(std::ptr::null::<()>()), false);
}

#[test]
fn check_4096() {
    assert_eq!(is_memory_valid(0x1000 as *const ()), false);
}

#[test]
fn check_kernel() {
    assert_eq!(is_memory_valid(0x80001000 as *const ()), false);
}

#[test]
fn check_variable() {
    let my_number = 1234;
    assert_eq!(is_memory_valid(&my_number), true);
}

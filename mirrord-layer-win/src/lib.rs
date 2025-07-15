mod dll;

use winapi::um::winnt::DLL_PROCESS_ATTACH;


entry_point!(|_, reason_for_call| {
    if reason_for_call != DLL_PROCESS_ATTACH {
        return false;
    }

    true
});

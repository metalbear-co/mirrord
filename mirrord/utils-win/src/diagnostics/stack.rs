//! Stack walks with frame sizes, and the CLI's stack-overflow report.
//!
//! A stack overflow names the function that ran out of stack, which is rarely the one that used it
//! up. What answers that is the size of every frame on the way down. [`walk`] recovers the frames
//! from the x64 unwind data, and a frame's size is the distance from its stack pointer to its
//! caller's: the numbers WinDbg's `kf` prints.
//!
//! [`walk`] is allocation-free, so the in-process crash handler can run it on a broken thread.
//! [`report_stack_overflows`] is for the CLI, which polls its whole runtime on the main thread: on
//! an overflow it prints the stack, named and with sizes, before the Rust runtime's own "has
//! overflowed its stack" abort. That lands in whatever captures the CLI's stderr, CI logs included.

use std::{
    ffi::c_void,
    fmt::Write as _,
    ops::Range,
    os::windows::ffi::OsStrExt,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, EXCEPTION_STACK_OVERFLOW, TRUE},
    System::{
        Diagnostics::Debug::{
            AddVectoredExceptionHandler, CONTEXT, EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS,
            IMAGE_RUNTIME_FUNCTION_ENTRY, RtlLookupFunctionEntry, RtlVirtualUnwind, SYMBOL_INFOW,
            SYMFLAG_PUBLIC_CODE, SYMOPT_DEFERRED_LOADS, SYMOPT_FAIL_CRITICAL_ERRORS,
            SYMOPT_NO_PROMPTS, SYMOPT_UNDNAME, SymCleanup, SymFromAddrW, SymInitializeW,
            SymSetOptions, UNW_FLAG_CHAININFO, UNW_FLAG_NHANDLER,
        },
        Threading::{
            CreateThread, GetCurrentProcess, STACK_SIZE_PARAM_IS_A_RESERVATION,
            SetThreadStackGuarantee, WaitForSingleObject,
        },
    },
};

use crate::modules::ModuleTable;

/// Stack the overflow handler keeps in reserve. It only starts a thread and waits on it.
const HANDLER_STACK_GUARANTEE: u32 = 32 * 1024;
/// Stack for the thread that writes the report. Symbolizing through `dbghelp` takes a lot.
const REPORT_THREAD_STACK: usize = 8 * 1024 * 1024;
/// How long the overflowing thread waits for its report before letting the process die anyway.
const REPORT_TIMEOUT_MS: u32 = 60_000;
/// The deepest stack the report walks.
const REPORT_FRAMES: usize = 4096;
/// Frames under this size are summed up in one line rather than listed.
const SMALL_FRAME: usize = 1024;
/// How many of the largest frames the report ranks.
const LARGEST_FRAMES: usize = 10;
/// Function names are cut to this many characters; generic instantiations run very long.
const NAME_WIDTH: usize = 160;
/// Longest symbol name read back from `dbghelp`, in UTF-16 units.
const MAX_SYMBOL_NAME: usize = 1024;

static ARMED: AtomicBool = AtomicBool::new(false);
/// Set by the first overflow, so a process reports once.
static REPORTED: AtomicBool = AtomicBool::new(false);
/// The overflowing thread's registers and stack bounds, for the report thread.
///
/// A copy, so the report never reads the exception's own context record, which is gone once the
/// handler returns.
static OVERFLOW: OnceLock<Overflow> = OnceLock::new();

struct Overflow {
    context: CONTEXT,
    stack: Range<usize>,
}

/// One frame of a walked stack.
#[derive(Clone, Copy, Default)]
pub struct Frame {
    /// The faulting instruction for the innermost frame, a return address for every other one.
    pub ip: usize,
    /// The stack pointer inside this frame.
    pub sp: usize,
}

/// The committed part of the calling thread's stack, from its thread environment block.
///
/// All of it is mapped, so a walk that reads only inside it never faults.
pub fn current_stack() -> Range<usize> {
    let (base, limit): (usize, usize);
    // SAFETY: on x64 Windows, `gs` points at the current thread's TEB, which starts with `NT_TIB`:
    // `StackBase` at offset 8, `StackLimit` at offset 0x10.
    unsafe {
        std::arch::asm!(
            "mov {base}, gs:[0x08]",
            "mov {limit}, gs:[0x10]",
            base = out(reg) base,
            limit = out(reg) limit,
            options(nostack, readonly, preserves_flags),
        );
    }
    limit..base
}

/// Walks the stack that `context` describes, innermost frame first, into `frames`.
///
/// Allocation-free. Stops at the outermost frame, when `frames` is full, or where the unwind data
/// leads somewhere a caller cannot be: below the previous frame, or outside `stack`. The stack a
/// crash walks may be corrupt, and these bounds keep the walk from faulting on it.
///
/// # Safety
///
/// `context` must describe a stack of this process that does not change during the walk: the
/// calling thread's own, or that of a thread blocked until the walk is done. `stack` must be that
/// thread's, from [`current_stack`].
///
/// # Returns
///
/// How many frames were written.
pub unsafe fn walk(context: &CONTEXT, stack: Range<usize>, frames: &mut [Frame]) -> usize {
    let mut context = *context;
    let mut count = 0;

    for slot in frames.iter_mut() {
        let sp = context.Rsp as usize;
        let in_stack = sp >= stack.start && sp.checked_add(8).is_some_and(|end| end <= stack.end);
        if context.Rip == 0 || !in_stack {
            break;
        }
        *slot = Frame {
            ip: context.Rip as usize,
            sp: context.Rsp as usize,
        };
        count += 1;

        let callee_sp = context.Rsp;
        let mut image_base = 0;
        let entry =
            unsafe { RtlLookupFunctionEntry(context.Rip, &mut image_base, std::ptr::null_mut()) };
        if entry.is_null() {
            // A leaf function has no unwind data because it never moves rsp, so its return
            // address is on top of the stack.
            context.Rip = unsafe { *(context.Rsp as *const u64) };
            context.Rsp += 8;
        } else {
            // `RtlVirtualUnwind` reads the saved registers through the frame register when the
            // function has one, and on a corrupt stack that register can point anywhere.
            if !unsafe { frame_register_in_stack(&context, image_base, entry, &stack) } {
                break;
            }
            let mut handler_data = std::ptr::null_mut();
            let mut establisher_frame = 0;
            unsafe {
                RtlVirtualUnwind(
                    UNW_FLAG_NHANDLER,
                    image_base,
                    context.Rip,
                    entry,
                    &mut context,
                    &mut handler_data,
                    &mut establisher_frame,
                    std::ptr::null_mut(),
                )
            };
        }

        // The stack grows down, so a caller sits above its callee; anything else is a lost walk.
        if context.Rsp <= callee_sp {
            break;
        }
    }

    count
}

/// Whether the frame that `entry`'s unwind data sets up with a frame register, if it has one,
/// starts inside `stack`.
///
/// Follows `UNWIND_INFO` as the x64 ABI lays it out: byte 0 holds the flags in its top 5 bits,
/// byte 2 the unwind code count, and byte 3 the frame register in its low 4 bits with the frame's
/// offset below it, in 16-byte units, in its high 4. A chained entry follows the unwind codes.
///
/// # Safety
///
/// `entry` must come from `RtlLookupFunctionEntry`, with `image_base` its module's base.
unsafe fn frame_register_in_stack(
    context: &CONTEXT,
    image_base: u64,
    entry: *const IMAGE_RUNTIME_FUNCTION_ENTRY,
    stack: &Range<usize>,
) -> bool {
    let mut entry = unsafe { entry.read_unaligned() };
    // Real chains are a few entries long; the bound only stops a loop in a corrupt image.
    for _ in 0..32 {
        let info = (image_base + u64::from(unsafe { entry.Anonymous.UnwindData })) as *const u8;
        let [flags, _, code_count, frame] = unsafe { info.cast::<[u8; 4]>().read_unaligned() };

        let register = frame & 0x0f;
        if register != 0 {
            let offset = u64::from(frame >> 4) * 16;
            let in_stack = register_value(context, register)
                .checked_sub(offset)
                .is_some_and(|frame| stack.contains(&(frame as usize)));
            if !in_stack {
                return false;
            }
        }

        if u32::from(flags >> 3) & UNW_FLAG_CHAININFO == 0 {
            return true;
        }
        // The codes take 2 bytes each, and their count is rounded up to even.
        let codes = usize::from(code_count.next_multiple_of(2)) * 2;
        entry = unsafe {
            info.add(4 + codes)
                .cast::<IMAGE_RUNTIME_FUNCTION_ENTRY>()
                .read_unaligned()
        };
    }
    false
}

/// The general-purpose register that unwind data numbers `number`, in the x64 encoding's order.
fn register_value(context: &CONTEXT, number: u8) -> u64 {
    match number {
        0 => context.Rax,
        1 => context.Rcx,
        2 => context.Rdx,
        3 => context.Rbx,
        4 => context.Rsp,
        5 => context.Rbp,
        6 => context.Rsi,
        7 => context.Rdi,
        8 => context.R8,
        9 => context.R9,
        10 => context.R10,
        11 => context.R11,
        12 => context.R12,
        13 => context.R13,
        14 => context.R14,
        15 => context.R15,
        // Unwind data has 4 bits for it. Zero is outside every stack, so the walk stops.
        _ => 0,
    }
}

/// The stack the frame at `index` uses: the distance to its caller's stack pointer.
///
/// The last frame in `frames` has no caller there, so it reports 0.
pub fn frame_size(frames: &[Frame], index: usize) -> usize {
    match (frames.get(index), frames.get(index + 1)) {
        (Some(frame), Some(caller)) => caller.sp.saturating_sub(frame.sp),
        _ => 0,
    }
}

/// Prints the stack, every frame with its size, when a thread of this process overflows its stack.
///
/// Call it first thing in `main`. The handler is process-wide, but the stack it runs on is the
/// overflowing thread's guaranteed reserve, which this sets for the calling thread only; other
/// threads rely on the reserve the Rust runtime gives them, which is enough to start the report.
pub fn report_stack_overflows() {
    if ARMED.swap(true, Ordering::SeqCst) {
        return;
    }

    let mut guarantee = HANDLER_STACK_GUARANTEE;
    unsafe {
        SetThreadStackGuarantee(&mut guarantee);
        // First in line, ahead of the Rust runtime's handler that prints its message and aborts.
        AddVectoredExceptionHandler(1, Some(overflow_handler));
    }
}

/// Runs on the overflowing thread, before the Rust runtime's handler.
unsafe extern "system" fn overflow_handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    let code = unsafe { (*(*info).ExceptionRecord).ExceptionCode };
    if code != EXCEPTION_STACK_OVERFLOW || REPORTED.swap(true, Ordering::SeqCst) {
        return EXCEPTION_CONTINUE_SEARCH;
    }

    let _ = OVERFLOW.set(Overflow {
        context: unsafe { *(*info).ContextRecord },
        stack: current_stack(),
    });

    // This thread is out of stack, so the report is made on a fresh one. The stack it walks stays
    // put while this thread waits.
    unsafe {
        let thread = CreateThread(
            std::ptr::null(),
            REPORT_THREAD_STACK,
            Some(report_thread),
            std::ptr::null(),
            STACK_SIZE_PARAM_IS_A_RESERVATION,
            std::ptr::null_mut(),
        );
        if !thread.is_null() {
            WaitForSingleObject(thread, REPORT_TIMEOUT_MS);
            CloseHandle(thread);
        }
    }

    EXCEPTION_CONTINUE_SEARCH
}

/// Walks, names and prints the overflowing thread's stack.
unsafe extern "system" fn report_thread(_: *mut c_void) -> u32 {
    let Some(overflow) = OVERFLOW.get() else {
        return 0;
    };

    let mut frames = vec![Frame::default(); REPORT_FRAMES];
    let count = unsafe { walk(&overflow.context, overflow.stack.clone(), &mut frames) };
    frames.truncate(count);

    let names = symbolize(&frames);
    let report = format_report(&frames, &names, overflow.context.Rax as usize);
    unsafe { super::crash::write_stderr(report.as_bytes()) };
    0
}

/// Names the function of each frame.
///
/// Names come from the PDB next to the executable, which debug and CI builds have. Without it, a
/// frame is named by module and offset, which the PDB of the same build can resolve later.
fn symbolize(frames: &[Frame]) -> Vec<String> {
    let process = unsafe { GetCurrentProcess() };

    let search_path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.as_os_str().to_owned()))
        .map(|dir| dir.encode_wide().chain([0]).collect::<Vec<u16>>());
    let symbols = unsafe {
        SymSetOptions(
            SYMOPT_UNDNAME
                | SYMOPT_DEFERRED_LOADS
                | SYMOPT_FAIL_CRITICAL_ERRORS
                | SYMOPT_NO_PROMPTS,
        );
        SymInitializeW(
            process,
            search_path
                .as_ref()
                .map_or(std::ptr::null(), |path| path.as_ptr()),
            TRUE,
        ) != 0
    };
    let modules = ModuleTable::capture();

    let names = frames
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            // A return address points past the call, which may be the next function already.
            let ip = if index == 0 { frame.ip } else { frame.ip - 1 };
            let location = modules
                .resolve_offset(ip)
                .map(|(module, offset)| format!("{module}+{offset:#x}"));
            match (
                symbols.then(|| symbol_name(process, ip)).flatten(),
                location,
            ) {
                (
                    Some(Symbol {
                        name,
                        public: false,
                    }),
                    _,
                ) => name,
                (Some(Symbol { name, public: true }), Some(location)) => {
                    format!("{name} (nearest public symbol, {location})")
                }
                (Some(Symbol { name, public: true }), None) => {
                    format!("{name} (nearest public symbol)")
                }
                (None, Some(location)) => location,
                (None, None) => format!("{ip:#018x}"),
            }
        })
        .collect();

    if symbols {
        unsafe { SymCleanup(process) };
    }
    names
}

/// A frame's function name, as `dbghelp` found it.
struct Symbol {
    name: String,
    /// Found among the public symbols only, which is all a PDB without debug info has. A function
    /// without one of its own then gets the name of the nearest public one before it, so the name
    /// may be wrong; its module and offset are what a PDB with debug info can resolve exactly.
    public: bool,
}

/// The function containing `address`, from loaded symbols.
///
/// Public symbols hold mangled names, so names are demangled here.
fn symbol_name(process: *mut c_void, address: usize) -> Option<Symbol> {
    /// `SYMBOL_INFOW` ends in a one-element name array that is really the start of a longer one.
    #[repr(C)]
    struct SymbolInfo {
        info: SYMBOL_INFOW,
        name: [u16; MAX_SYMBOL_NAME],
    }

    let mut symbol: SymbolInfo = unsafe { std::mem::zeroed() };
    symbol.info.SizeOfStruct = size_of::<SYMBOL_INFOW>() as u32;
    symbol.info.MaxNameLen = MAX_SYMBOL_NAME as u32 + 1;

    let mut displacement = 0;
    let found =
        unsafe { SymFromAddrW(process, address as u64, &mut displacement, &mut symbol.info) } != 0;
    if !found {
        return None;
    }

    let length = (symbol.info.NameLen as usize).min(MAX_SYMBOL_NAME + 1);
    let name = unsafe { std::slice::from_raw_parts(symbol.info.Name.as_ptr(), length) };
    let name = String::from_utf16_lossy(name);
    Some(Symbol {
        // `{:#}` leaves out the crate hashes; anything that isn't a Rust symbol comes back
        // unchanged.
        name: format!("{:#}", rustc_demangle::demangle(&name)),
        public: symbol.info.Flags & SYMFLAG_PUBLIC_CODE != 0,
    })
}

/// Formats the report: the stack with sizes, then the largest frames.
///
/// # Arguments
///
/// * `requested` - the overflowing thread's rax, which holds the size of the frame being allocated
///   when the overflow hit in `__chkstk`.
fn format_report(frames: &[Frame], names: &[String], requested: usize) -> String {
    let sizes: Vec<usize> = (0..frames.len())
        .map(|index| frame_size(frames, index))
        .collect();
    let total: usize = sizes.iter().sum();

    let mut report = String::new();
    let _ = writeln!(
        report,
        "\nmirrord: stack overflow. The stack, innermost frame first ({} across {} frames):\n",
        kib(total),
        frames.len()
    );

    let (mut small_count, mut small_bytes) = (0, 0);
    for (size, name) in sizes.iter().zip(names) {
        if *size < SMALL_FRAME {
            small_count += 1;
            small_bytes += size;
            continue;
        }
        let _ = writeln!(report, "{:>12}  {}", kib(*size), cut(name));
    }
    if small_count > 0 {
        let _ = writeln!(
            report,
            "{:>12}  ({small_count} frames under {} each)",
            kib(small_bytes),
            kib(SMALL_FRAME)
        );
    }

    // Most overflows hit in `__chkstk`, the probe a function calls to commit a frame larger than
    // a page. It takes that frame's size in rax, so the function asking for it is the next frame.
    if names.first().is_some_and(|name| name.contains("__chkstk"))
        && let Some(caller) = names.get(1)
    {
        let _ = writeln!(
            report,
            "\nThe overflow hit while {} was allocating a {} frame.",
            cut(caller),
            kib(requested)
        );
    }

    let mut largest: Vec<(usize, &String)> = sizes.iter().copied().zip(names).collect();
    largest.sort_by(|(left, _), (right, _)| right.cmp(left));
    let _ = writeln!(report, "\nLargest frames:\n");
    for (size, name) in largest.into_iter().take(LARGEST_FRAMES) {
        let _ = writeln!(
            report,
            "{:>12}  {:>3}%  {}",
            kib(size),
            size * 100 / total.max(1),
            cut(name)
        );
    }
    report.push('\n');
    report
}

fn kib(bytes: usize) -> String {
    format!("{:.1} KiB", bytes as f64 / 1024.0)
}

/// Cuts a function name to [`NAME_WIDTH`] characters.
fn cut(name: &str) -> String {
    if name.chars().count() <= NAME_WIDTH {
        return name.to_owned();
    }
    let mut cut: String = name.chars().take(NAME_WIDTH - 3).collect();
    cut.push_str("...");
    cut
}

#[cfg(test)]
mod tests {
    use std::hint::black_box;

    use windows_sys::Win32::System::Diagnostics::Debug::RtlCaptureContext;

    use super::*;

    /// A local this aligned makes the compiler realign the frame and address it through `rbp`,
    /// which the unwind data then names as the frame register.
    #[repr(align(64))]
    struct Aligned(#[allow(dead_code)] [u8; 64]);

    /// `RtlCaptureContext` saves with `fxsave`, which needs the 16-byte alignment that Windows
    /// declares `CONTEXT` with and `windows-sys` leaves out.
    #[repr(C, align(16))]
    struct AlignedContext(CONTEXT);

    /// Walks the stack from inside a frame that has a frame register, with the walked context's
    /// `rbp` replaced when one is given.
    #[inline(never)]
    fn walk_from_frame_with_frame_register(rbp: Option<u64>) -> usize {
        let aligned = black_box(Aligned([0; 64]));
        let mut context: AlignedContext = unsafe { std::mem::zeroed() };
        unsafe { RtlCaptureContext(&mut context.0) };
        if let Some(rbp) = rbp {
            context.0.Rbp = rbp;
        }
        let mut frames = [Frame::default(); 64];
        let count = unsafe { walk(&context.0, current_stack(), &mut frames) };
        black_box(&aligned);
        count
    }

    #[test]
    fn walks_through_a_frame_register() {
        assert!(walk_from_frame_with_frame_register(None) > 1);
    }

    /// The unwind reads through `rbp`, so before the bound this one faulted at 0x1000.
    #[test]
    fn stops_at_a_frame_register_outside_the_stack() {
        assert_eq!(walk_from_frame_with_frame_register(Some(0x1000)), 1);
    }
}

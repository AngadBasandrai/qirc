#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::{ptr, slice};

use qirc::diag::SourceFile;
use qirc::driver::{self, Output};
use qirc::{explain, surface};

const LINE5: &str = include_str!("../../examples/line5.cal");

thread_local! {
    /// Calibration files the host has defined by name with [`define`], beside the built in ones.
    static FILES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// Allocates `len` bytes in WebAssembly linear memory for the host.
///
/// The host owns the returned allocation and must eventually pass the pointer and the exact
/// same length to [`release`], exactly once. The pointer remains valid until that call.
#[unsafe(no_mangle)]
extern "C" fn allocate(len: usize) -> *mut u8 {
    Box::into_raw(vec![0u8; len].into_boxed_slice()).cast()
}

/// Releases an allocation previously returned to the host by [`allocate`] or [`run`].
///
/// # Safety
///
/// `ptr` must be the pointer returned by one of those functions, `len` must be that allocation's
/// exact length, and the allocation must not have been released before. After this call the host
/// must not read, write, or release the allocation again.
#[unsafe(no_mangle)]
unsafe extern "C" fn release(ptr: *mut u8, len: usize) {
    // SAFETY: The caller guarantees that this is the original pointer and length from
    // `Box::into_raw`, with unique ownership returned to Rust exactly once.
    drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(ptr, len)) });
}

/// Defines a calibration file that later runs can name with `--calibration`.
///
/// # Safety
///
/// Both pointers must be non-null, aligned, and valid to read for their lengths for the duration
/// of this call. This function copies both buffers and does not release either.
#[unsafe(no_mangle)]
unsafe extern "C" fn define(name: *const u8, name_len: usize, text: *const u8, text_len: usize) {
    let read = |ptr, len| {
        // SAFETY: The caller guarantees that both regions are readable for their given lengths.
        String::from_utf8_lossy(unsafe { slice::from_raw_parts(ptr, len) }).into_owned()
    };
    let (name, text) = (read(name, name_len), read(text, text_len));
    FILES.with(|files| files.borrow_mut().insert(name, text));
}

/// Runs the compiler on UTF-8 source and command-line argument buffers owned by the host.
///
/// The returned allocation starts with three little-endian `u32` values: exit code, stdout
/// length, and stderr length. The two byte strings follow the 12-byte header. The host owns this
/// allocation and must pass it to [`release`] exactly once with length
/// `12 + stdout_length + stderr_length`.
///
/// # Safety
///
/// Each input pointer must be non-null, aligned, and valid to read for its corresponding length,
/// including when the length is zero. Both input allocations must remain alive and unmodified for
/// the duration of this call. This function borrows but does not release either input.
#[unsafe(no_mangle)]
unsafe extern "C" fn run(
    source: *const u8,
    source_len: usize,
    args: *const u8,
    args_len: usize,
) -> *mut u8 {
    let text = |ptr, len| {
        // SAFETY: The caller guarantees that both input regions are readable for their given
        // lengths and remain alive for this call. `from_utf8_lossy` does not retain the slice.
        String::from_utf8_lossy(unsafe { slice::from_raw_parts(ptr, len) })
    };
    let mut output = Output::default();
    let code = execute(
        &text(source, source_len),
        &text(args, args_len),
        &mut output,
    );

    let mut reply = Vec::new();
    for word in [
        code as u32,
        output.stdout.len() as u32,
        output.stderr.len() as u32,
    ] {
        reply.extend(word.to_le_bytes());
    }
    reply.extend(output.stdout.bytes());
    reply.extend(output.stderr.bytes());
    Box::into_raw(reply.into_boxed_slice()).cast()
}

fn execute(source: &str, args: &str, output: &mut Output) -> i32 {
    let words: Vec<String> = args.split_whitespace().map(String::from).collect();
    match words.first().map(String::as_str) {
        Some("surface") => {
            return match surface::command(&words[1..]) {
                Ok(text) => {
                    output.stdout.push_str(&text);
                    0
                }
                Err(message) => {
                    output.stderr.push_str(&format!("error: {message}\n"));
                    2
                }
            };
        }
        Some("explain") if words.len() == 2 => {
            return match explain::explain(&words[1]) {
                Some(text) => {
                    output.stdout.push_str(text);
                    output.stdout.push('\n');
                    0
                }
                None => {
                    output
                        .stderr
                        .push_str(&format!("error: no error code `{}`\n", words[1]));
                    2
                }
            };
        }
        _ => {}
    }
    let defined = |path: &str| FILES.with(|files| files.borrow().get(path).cloned());
    let device = |path: &str| match (defined(path), path) {
        (Some(text), _) => Ok(text),
        (None, "line5.cal") => Ok(LINE5.to_string()),
        (None, "detuned.cal") => Ok((0..5).fold(LINE5.to_string(), |text, q| {
            text + &format!("detuning {q} 300\n")
        })),
        _ => Err(format!(
            "the playground only has the calibrations line5.cal and detuned.cal, not {path}"
        )),
    };
    let options = match driver::parse_args_with(&words, device) {
        Ok(options) => options,
        Err(message) if message.is_empty() => {
            output.stdout.push_str(driver::USAGE);
            return 0;
        }
        Err(message) => {
            output.stderr.push_str(&format!("error: {message}\n"));
            return 2;
        }
    };

    let file = SourceFile::new(options.input.display().to_string(), source);
    driver::run_source(&options, &file, None, output)
}

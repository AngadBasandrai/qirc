use std::{ptr, slice};

use qirc::diag::SourceFile;
use qirc::driver::{self, Output};

const LINE5: &str = include_str!("../../examples/line5.cal");

#[unsafe(no_mangle)]
extern "C" fn allocate(len: usize) -> *mut u8 {
    Box::into_raw(vec![0u8; len].into_boxed_slice()).cast()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn release(ptr: *mut u8, len: usize) {
    drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(ptr, len)) });
}

#[unsafe(no_mangle)]
unsafe extern "C" fn run(
    source: *const u8,
    source_len: usize,
    args: *const u8,
    args_len: usize,
) -> *mut u8 {
    let text = |ptr, len| String::from_utf8_lossy(unsafe { slice::from_raw_parts(ptr, len) });
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
    let device = |path: &str| match path {
        "line5.cal" => Ok(LINE5.to_string()),
        _ => Err(format!(
            "the playground only has the calibration line5.cal, not {path}"
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

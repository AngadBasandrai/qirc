#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

use std::env;
use std::io::{self, BufReader};
use std::process;

use qirc::{driver, explain, lsp, progress, surface};

#[cfg(windows)]
fn stop() {
    if progress::interrupt() {
        process::exit(130);
    }
}

#[cfg(windows)]
fn listen() {
    #[link(name = "kernel32")]
    // This declaration matches the Win32 console control API signature.
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }
    // The operating system invokes this callback with the declared ABI.
    unsafe extern "system" fn handle(event: u32) -> i32 {
        if event > 1 {
            return 0;
        }
        stop();
        1
    }
    // SAFETY: `handle` has static lifetime and the ABI and signature required by Windows.
    unsafe {
        SetConsoleCtrlHandler(Some(handle), 1);
    }
}

#[cfg(unix)]
fn listen() {
    const SIGINT: i32 = 2;

    // These declarations match the POSIX signal and process-exit APIs.
    unsafe extern "C" {
        fn signal(signal: i32, handler: extern "C" fn(i32)) -> usize;
        fn _exit(status: i32);
    }
    extern "C" fn handle(_: i32) {
        if progress::interrupt() {
            // SAFETY: `_exit` is async-signal-safe and terminates immediately without running
            // Rust destructors. This is only used for a second interrupt.
            unsafe { _exit(130) };
        }
    }
    // SAFETY: `SIGINT` is the interrupt signal on supported Unix targets, and `handle` has C ABI,
    // static lifetime, and performs only an atomic swap or the async-signal-safe `_exit` operation.
    unsafe {
        signal(SIGINT, handle);
    }
}

#[cfg(not(any(windows, unix)))]
fn listen() {}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() {
        print!("{}", driver::USAGE);
        process::exit(2);
    }

    if args.first().is_some_and(|a| a == "surface") {
        listen();
        match surface::command(&args[1..]) {
            Ok(text) => print!("{text}"),
            Err(message) => {
                eprintln!("error: {message}");
                process::exit(2);
            }
        }
        return;
    }

    if args == ["explain"] {
        for code in explain::codes() {
            let summary = explain::explain(code)
                .and_then(|text| text.lines().next())
                .unwrap_or_default();
            println!("{code}  {summary}");
        }
        println!();
        println!("run `qirc explain <code>` for the full explanation");
        return;
    }

    if let [command, code] = &args[..]
        && command == "explain"
    {
        match explain::explain(code) {
            Some(text) => println!("{text}"),
            None => {
                let codes: Vec<&str> = explain::codes().collect();
                eprintln!(
                    "error: no error code `{code}`, the codes are {}",
                    codes.join(", ")
                );
                process::exit(2);
            }
        }
        return;
    }

    if args == ["lsp"] {
        if let Err(error) = lsp::serve(BufReader::new(io::stdin()), io::stdout().lock()) {
            eprintln!("error: {error}");
            process::exit(1);
        }
        return;
    }

    listen();
    match driver::parse_args(&args) {
        Ok(options) => process::exit(driver::run(options)),
        Err(message) => {
            if !message.is_empty() {
                eprintln!("error: {message}");
                eprintln!();
            }
            print!("{}", driver::USAGE);
            process::exit(if message.is_empty() { 0 } else { 2 });
        }
    }
}

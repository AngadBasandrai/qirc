use std::env;
use std::io::{self, BufReader};
use std::process;

use qirc::{driver, explain, lsp, progress, surface};

fn stop() {
    if progress::interrupt() {
        process::exit(130);
    }
}

#[cfg(windows)]
fn listen() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }
    unsafe extern "system" fn handle(event: u32) -> i32 {
        if event > 1 {
            return 0;
        }
        stop();
        1
    }
    unsafe {
        SetConsoleCtrlHandler(Some(handle), 1);
    }
}

#[cfg(unix)]
fn listen() {
    unsafe extern "C" {
        fn signal(signal: i32, handler: extern "C" fn(i32)) -> usize;
    }
    extern "C" fn handle(_: i32) {
        stop();
    }
    unsafe {
        signal(2, handle);
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

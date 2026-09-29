use std::env;
use std::io;
use std::process;

use qirc::{driver, lsp};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() {
        print!("{}", driver::USAGE);
        process::exit(2);
    }

    if args == ["lsp"] {
        if let Err(error) = lsp::serve(io::stdin().lock(), io::stdout().lock()) {
            eprintln!("error: {error}");
            process::exit(1);
        }
        return;
    }

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

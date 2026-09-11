use qirc::driver;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        print!("{}", driver::USAGE);
        std::process::exit(2);
    }

    match driver::parse_args(&args) {
        Ok(options) => std::process::exit(driver::run(options)),
        Err(message) => {
            if !message.is_empty() {
                eprintln!("error: {message}");
                eprintln!();
            }
            print!("{}", driver::USAGE);
            std::process::exit(if message.is_empty() { 0 } else { 2 });
        }
    }
}

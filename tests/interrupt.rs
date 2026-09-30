use qirc::diag::SourceFile;
use qirc::driver::{self, Output};
use qirc::progress;

const BELL: &str = "OPENQASM 3.0;
include \"stdgates.inc\";
qubit[2] q;
bit[2] c;
h q[0];
cx q[0], q[1];
c = measure q;
";

#[test]
fn stops() {
    assert!(!progress::interrupt());
    let args: Vec<String> = ["bell.qasm", "--shots", "1000"].map(String::from).to_vec();
    let options = driver::parse_args_with(&args, |_| Err("no files".into())).unwrap();
    let mut output = Output::default();
    let code = driver::run_source(
        &options,
        &SourceFile::new("bell.qasm", BELL),
        None,
        &mut output,
    );
    assert_eq!(code, 0, "{}", output.stderr);
    assert!(
        output
            .stderr
            .contains("stopped by Ctrl+C after 0 of 1000 shots"),
        "{}",
        output.stderr
    );
}

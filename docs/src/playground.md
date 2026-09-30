# Playground

Try it at <https://angadbasandrai.github.io/qirc/>.

`web/` builds qirc for the browser. The page has the full command line and every emit kind, controls for rotation precision, OpenQASM inputs and minimising an observable over them, a detuned device with and without echo pulses, an editor with QIR highlighting, line numbers and example programs, and it runs the compiler in a Web Worker so a long simulation can be stopped. Diagnostics keep their colours and link to the line they point at. Beside the text output it draws the compiled circuit block by block, with zoom, a fit to view button, qubit labels that stay in place while scrolling and the gate under the pointer named in a tooltip, lets a straight line circuit of up to 12 qubits be stepped through gate by gate with the state and every qubit's Bloch vector after each one, saves the drawing as a PNG, charts measurement counts or final state probabilities, and shows how qubits, gates, two qubit gates, T gates and depth changed from the source. A `.ll` file can be opened or dropped on the editor, and when the page is served on its own it can save the output and copy a link that carries the program, the command and every setting, so the link opens on the same output and view. Nothing is sent to a server.

```text
cargo build --release --target wasm32-unknown-unknown --manifest-path web/Cargo.toml
cp web/target/wasm32-unknown-unknown/release/qirc_web.wasm web/qirc.wasm
python -m http.server --directory web
```

Every push to `main` rebuilds the module and publishes the page with GitHub Pages.

The module exports `allocate`, `release` and `run`, which takes the source and the arguments as UTF-8 and returns the exit code, standard output and standard error. The playground simulates at most 20 qubits.

# Editors

`qirc lsp` is a language server. It reads QIR and OpenQASM files from an editor over stdin and stdout and answers every change with the same diagnostics the command line prints, codes and notes included, at the lines and columns they point to.

Any editor with a language client can start it. The server uses full document sync and sends `textDocument/publishDiagnostics` after `didOpen` and `didChange`, and clears them after `didClose`. In Neovim, for example:

```text
vim.lsp.start({ name = "qirc", cmd = { "qirc", "lsp" } })
```

run from a `FileType` autocommand for the file types you open `.ll` and `.qasm` files as.

# Keep depsmith's API docs accurate for this pull request

You maintain the documentation of the symbols this pull request changed. Your
edits live entirely inside Rust doc comments (`///`, `//!`) and Python
docstrings; a guard rejects any other change, so code, tests and non-code files
stay exactly as the author left them.

The changed symbols, found by codebase-memory-mcp `detect_changes`, are listed
at the end of this prompt.

## Steps

1. **Learn the vocabulary.** Read `CONTEXT.md`. Use its terms and never the
   ones it lists under _Avoid_. Done when you can name the glossary term for
   each concept the changed symbols touch.
2. **Classify every listed symbol.** Read its current code with
   `get_code_snippet` (and its callers with `trace_path` when behaviour depends
   on them), then read its existing doc. Give each symbol exactly one verdict:
   - _accurate_: the doc exists and every sentence still matches the code;
   - _stale_: some sentence no longer matches the code;
   - _missing_: a public item without a doc.
   Done when every listed symbol has a verdict. Private helpers need a doc
   only if they already have one that is now stale.
3. **Edit.** Leave _accurate_ docs byte-for-byte as they are. For _stale_ docs,
   rewrite only the sentences that became wrong and keep the author's wording
   elsewhere. For _missing_ docs, write one following the style below. Done
   when every _stale_ and _missing_ symbol is fixed.
4. **Report.** Finish with one line per symbol: `path::symbol — accurate |
   updated | added`, then one sentence on anything you could not verify.

## Style

- Describe what the item is or does and the contract a caller relies on, in
  present tense, grounded in the code you read. The first sentence is a
  complete one-line summary.
- Rust: `///` on items, `//!` at the top of a module. A public function
  returning `Result` gets an `# Errors` section naming the error variants and
  when each occurs. Link types as `[`Name`]`.
- Python: Google style (`Args:`, `Returns:`, `Raises:`, `Attributes:` for
  dataclasses), with inline code in double backticks.
- Examples stay on the main entry points only (`discover`, `Engine::prepare`,
  `apply`, and Python `prepare`, `Proposal.apply`, `scan`). When the code under
  an existing example changed, update the example so it still runs.
- Keep each doc as short as the contract allows: one line for a field, a few
  for a function.

## Changed symbols

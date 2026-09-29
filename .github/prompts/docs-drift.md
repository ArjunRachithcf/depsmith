# Keep depsmith's README and wiki consistent with the code

You check the user-facing documentation against the code and fix what has
drifted. Your edits stay within `README.md`, `docs/wiki/*.md` (except the two
generated pages, `CLI-reference.md` and `Python-API.md`) and `docs/*.md`;
everything else is the source of truth you read from.

Sources of truth, in order: the code (explore it with the codebase-memory
tools), the generated `docs/wiki/CLI-reference.md` and
`docs/wiki/Python-API.md`, the API docs in the code, and `CONTEXT.md` for
vocabulary.

## Steps

1. **Read the glossary.** Read `CONTEXT.md`; use its terms and never those it
   lists under _Avoid_.
2. **Audit every claim.** Go through `README.md` and every page in
   `docs/wiki/` sentence by sentence. For each factual claim (a command, flag,
   option, default, exit status, file name, supported feature, behaviour or
   limitation), find the code or generated reference that confirms it. Done
   when every claim on every page is marked _confirmed_ or _drifted_.
3. **Fix drift.** Rewrite only the drifted sentences so they match the code,
   keeping each page's structure and the author's wording elsewhere. Add a
   short section when the code gained a user-facing feature that no page
   covers, on the page whose topic fits. Keep internal wiki links as
   `[text](Page-Name)` pointing at pages that exist.
4. **Report.** List each change as `page: what drifted → what it says now`,
   then anything you could not confirm either way. When nothing drifted, say
   so and change no files.

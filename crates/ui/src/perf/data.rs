//! Synthetic data sized like a busy real profile. Deterministic (no RNG).

/// A markdown plan of roughly `target_bytes`: headings, paragraphs, lists,
/// tables, code fences and links, repeated with varying numbers.
pub fn markdown_plan(target_bytes: usize) -> String {
    let mut out = String::with_capacity(target_bytes + 4096);
    out.push_str("# Synthetic plan\n\n");
    let mut n = 0;
    while out.len() < target_bytes {
        n += 1;
        out.push_str(&format!("## Section {n}: rollout step\n\n"));
        out.push_str(&format!(
            "This section ({n}) describes the **rollout** of the *receiving* flow. It links to \
             [the contract](https://example.com/contract/{n}) and mentions `Registry::apply_op` \
             and `WorkItemView` inline. The paragraph is long enough to wrap across several lines \
             in a normal-width pane so that layout cost is realistic, and it keeps going for a \
             while with more prose about why this step exists and what it unblocks.\n\n"
        ));
        out.push_str("- first bullet with `code` and a [link](https://example.com)\n");
        out.push_str("- second bullet that is a little longer than the first, to wrap onto a second line when the pane is narrow\n");
        out.push_str("  - nested bullet one\n  - nested bullet two\n");
        out.push_str("- third bullet\n\n");
        out.push_str("| Column A | Column B | Column C |\n|---|---|---|\n");
        for r in 0..4 {
            out.push_str(&format!("| row {r} | value {n}-{r} | `x{r}` |\n"));
        }
        out.push('\n');
        out.push_str("```rust\nfn step(input: &Input) -> Result<Output, Error> {\n    let mut acc = Vec::new();\n    for item in input.items() {\n        acc.push(item.transform()?);\n    }\n    Ok(Output { acc })\n}\n```\n\n");
        out.push_str("1. numbered one\n2. numbered two\n3. numbered three\n\n> A block quote with some emphasis and a trailing remark.\n\n");
    }
    out
}

/// A unified diff of `lines` lines across files of ~250 lines each.
pub fn unified_diff(lines: usize) -> String {
    let mut out = String::new();
    let mut emitted = 0;
    let mut file = 0;
    while emitted < lines {
        file += 1;
        out.push_str(&format!(
            "diff --git a/src/mod{file}.rs b/src/mod{file}.rs\nindex 1111111..2222222 100644\n--- a/src/mod{file}.rs\n+++ b/src/mod{file}.rs\n"
        ));
        let mut line = 1;
        for hunk in 0..5 {
            out.push_str(&format!("@@ -{line},40 +{line},44 @@ fn hunk_{hunk}()\n"));
            for i in 0..40 {
                out.push_str(&match i % 10 {
                    3 => format!("-    let old_{i} = compute(a, {i});\n"),
                    4 => format!("+    let new_{i} = compute_v2(a, b, {i});\n"),
                    5 => format!("+    log::debug!(\"step {{}} of {file}\", {i});\n"),
                    _ => format!(
                        "     let value_{i} = other_function(arg_{i}, {line}); // context\n"
                    ),
                });
            }
            emitted += 42;
            line += 60;
        }
    }
    out
}

/// A tool-output blob (e.g. cargo/test output) of about `bytes`.
pub fn tool_output(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes + 100);
    let mut i = 0;
    while out.len() < bytes {
        i += 1;
        out.push_str(&format!(
            "   Compiling crate-{i} v0.{i}.0 (/work/repo/crates/crate-{i})\nwarning: unused variable `x{i}` --> src/lib.rs:{i}:9\n"
        ));
    }
    out
}

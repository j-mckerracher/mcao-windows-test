//! Reference differential for watcher-arm command-position analysis (DPES-966).
//!
//! `tests/fixtures/arm-wrapper-position.tsv` holds, for each corpus row, the result of
//! `bin/fm-arm-command-policy.mjs` `commandPosition`. This test runs the Rust port on the
//! same rows and requires an identical result. `tests/fm-arm-wrapper-differential.test.sh`
//! checks that the reference still produces the table and then runs this test.

use mcao_core::shell_command_pos::{Token, Word, command_position};

const TABLE: &str = include_str!("../../../tests/fixtures/arm-wrapper-position.tsv");

/// Formats one result in the same tab-separated layout as the reference.
fn port_row(words: &str) -> String {
    let tokens: Vec<Token> = words
        .split_whitespace()
        .map(|value| {
            Token::Word(Word {
                value: value.to_string(),
                literal: true,
                subs: Vec::new(),
                quoted: false,
                unquoted_expansion: false,
            })
        })
        .collect();
    let position = command_position(&tokens);
    let command = position.command().map_or("-", |word| word.value.as_str());
    let join = |items: &[String]| {
        if items.is_empty() {
            "-".to_string()
        } else {
            items.join(",")
        }
    };
    [
        position.index.to_string(),
        command.to_string(),
        join(&position.wrappers),
        position.prefix_assignments.to_string(),
        u8::from(position.unresolved_wrapper_option).to_string(),
        join(&position.wrapper_payloads),
    ]
    .join("\t")
}

#[test]
fn port_matches_reference_table() {
    let mut rows = 0;
    let mut mismatches = Vec::new();
    for line in TABLE.lines().filter(|line| !line.is_empty()) {
        let (words, expected) = line
            .split_once('\t')
            .expect("every table row is words, a tab, then the reference result");
        rows += 1;
        let got = port_row(words);
        if got != expected {
            mismatches.push(format!(
                "{words:?}\n  reference: {expected}\n  port:      {got}"
            ));
        }
    }
    assert!(rows >= 150, "differential table too small: {rows} rows");
    assert!(
        mismatches.is_empty(),
        "{} row(s) differ from the reference:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[test]
fn xargs_long_option_prefixes_match_reference() {
    let rows = [
        ("xargs --arg-f names.txt -I{} {}", "4\t{}\txargs\t0\t0\t-"),
        ("xargs --arg-f=names.txt -I{} {}", "3\t{}\txargs\t0\t0\t-"),
        ("xargs --no-run-if-em -I{} {}", "3\t{}\txargs\t0\t0\t-"),
        ("xargs --max 2 wc -l", "1\t--max\txargs\t0\t1\t-"),
        ("xargs --max-c 10 printf %s", "3\tprintf\txargs\t0\t0\t-"),
    ];
    for (input, expected) in rows {
        assert_eq!(port_row(input), expected, "{input}");
    }
}

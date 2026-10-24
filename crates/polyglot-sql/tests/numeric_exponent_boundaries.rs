use polyglot_sql::{generate, parse_one, DialectType, TokenType, Tokenizer};

#[test]
fn number_scanning_keeps_adjacent_end_keywords() {
    for prefix in ["", "/* π */"] {
        for number in ["13", "1.5", ".5", "1e3", "1E+3", "1e-3", ".5e2"] {
            let tokens = Tokenizer::default()
                .tokenize(&format!("{prefix}{number}END"))
                .unwrap();
            assert_eq!(tokens.len(), 2, "{tokens:?}");
            assert_eq!(tokens[0].token_type, TokenType::Number);
            assert_eq!(tokens[0].text, number);
            assert_eq!(tokens[1].token_type, TokenType::End);
            assert_eq!(tokens[1].text, "END");
        }
    }
}

#[test]
fn case_results_need_no_space_before_end_in_duckdb() {
    for number in ["13", "1.5", ".5", "1e3", "1E+3", "1e-3", ".5e2"] {
        let sql = format!("SELECT CASE WHEN TRUE THEN {number}END AS value");
        let compact = parse_one(&sql, DialectType::DuckDB).unwrap();
        let spaced = parse_one(&sql.replace("END", " END"), DialectType::DuckDB).unwrap();
        assert_eq!(
            generate(&compact, DialectType::DuckDB).unwrap(),
            generate(&spaced, DialectType::DuckDB).unwrap()
        );
    }
}

#[test]
fn scientific_notation_still_requires_exponent_digits() {
    for number in ["1e3", "1E+3", "1e-3", ".5e2", "1.e2"] {
        let tokens = Tokenizer::default().tokenize(number).unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].token_type, TokenType::Number);
        assert_eq!(tokens[0].text, number);
    }
    for sql in ["1e", "1E+", ".5e", ".5E-"] {
        let tokens = Tokenizer::default().tokenize(sql).unwrap();
        assert_eq!(tokens[0].token_type, TokenType::Number);
        assert!(!tokens[0].text.ends_with(['e', 'E', '+', '-']));
    }
}

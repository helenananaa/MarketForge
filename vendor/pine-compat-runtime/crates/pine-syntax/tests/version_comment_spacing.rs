use pine_syntax::{SourceFile, TokenKind, lex, parse_source};

fn lex_source(text: &str) -> pine_syntax::Lexed {
    lex(&SourceFile::new("version.pine", text))
}

fn version_tokens(text: &str) -> Vec<(u16, String)> {
    let source = SourceFile::new("version.pine", text);
    lex(&source)
        .tokens
        .into_iter()
        .filter_map(|token| match token.kind {
            TokenKind::VersionDirective(version) => Some((
                version,
                source.text()[token.span.start..token.span.end].to_owned(),
            )),
            _ => None,
        })
        .collect()
}

fn parsed_version(text: &str) -> Option<u16> {
    parse_source(&SourceFile::new("version.pine", text))
        .program
        .version
        .map(|version| version.version)
}

#[test]
fn zero_space_version_directive_still_matches() {
    let source = "//@version=6\nindicator(\"Demo\")\n";
    assert_eq!(version_tokens(source), vec![(6, "//@version=6".to_owned())]);
    assert_eq!(parsed_version(source), Some(6));
}

#[test]
fn one_space_between_comment_and_version_is_v6_not_v1() {
    let source = "// @version=6\nindicator(\"Demo\")\n";
    assert_eq!(
        version_tokens(source),
        vec![(6, "// @version=6".to_owned())]
    );
    assert_eq!(parsed_version(source), Some(6));
}

#[test]
fn multiple_spaces_and_tab_between_comment_and_version() {
    assert_eq!(
        version_tokens("//  @version=5\nindicator(\"Demo\")\n"),
        vec![(5, "//  @version=5".to_owned())]
    );
    assert_eq!(
        version_tokens("//   @version=6\nindicator(\"Demo\")\n"),
        vec![(6, "//   @version=6".to_owned())]
    );
    assert_eq!(
        version_tokens("//\t@version=6\nindicator(\"Demo\")\n"),
        vec![(6, "//\t@version=6".to_owned())]
    );
    assert_eq!(
        parsed_version("//  @version=5\nindicator(\"Demo\")\n"),
        Some(5)
    );
    assert_eq!(
        parsed_version("//\t@version=6\nindicator(\"Demo\")\n"),
        Some(6)
    );
}

#[test]
fn leading_indentation_before_version_comment_is_skipped() {
    let source = "    // @version=6\nindicator(\"Demo\")\n";
    let lexed = lex_source(source);
    assert!(
        !lexed
            .tokens
            .iter()
            .any(|token| matches!(token.kind, TokenKind::Indent | TokenKind::Dedent))
    );
    assert_eq!(
        version_tokens(source),
        vec![(6, "// @version=6".to_owned())]
    );
    assert_eq!(parsed_version(source), Some(6));

    let tabbed = "\t//@version=5\nindicator(\"Demo\")\n";
    assert_eq!(version_tokens(tabbed), vec![(5, "//@version=5".to_owned())]);
}

#[test]
fn crlf_version_directive_keeps_payload_and_span() {
    let source = "// @version=6\r\nindicator(\"Demo\")\r\n";
    assert_eq!(
        version_tokens(source),
        vec![(6, "// @version=6\r".to_owned())]
    );
    assert_eq!(parsed_version(source), Some(6));
}

#[test]
fn ordinary_comments_and_extra_prefix_are_not_version_directives() {
    for source in [
        "// see @version=6 later\nindicator(\"Demo\")\n",
        "///@version=6\nindicator(\"Demo\")\n",
        "//@@version=6\nindicator(\"Demo\")\n",
        "// @ @version=6\nindicator(\"Demo\")\n",
        "//@xversion=6\nindicator(\"Demo\")\n",
        "// @versionx=6\nindicator(\"Demo\")\n",
    ] {
        assert!(
            version_tokens(source).is_empty(),
            "unexpected version token in {source:?}"
        );
        assert_eq!(parsed_version(source), None, "{source:?}");
        assert!(lex_source(source).diagnostics.is_empty(), "{source:?}");
    }
}

#[test]
fn malformed_numeric_payload_still_emits_lex_version_error() {
    for source in [
        "//@version=\nindicator(\"Demo\")\n",
        "// @version=abc\nindicator(\"Demo\")\n",
        "// @version=6.5\nindicator(\"Demo\")\n",
        "//\t@version=99999\nindicator(\"Demo\")\n",
    ] {
        let lexed = lex_source(source);
        assert!(
            version_tokens(source).is_empty(),
            "unexpected version token in {source:?}"
        );
        assert_eq!(
            lexed
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<Vec<_>>(),
            vec!["E_LEX_VERSION"],
            "{source:?}"
        );
        assert_eq!(
            source[lexed.diagnostics[0].span.start..lexed.diagnostics[0].span.end]
                .trim_end_matches(['\r', '\n']),
            source.lines().next().unwrap().trim_end_matches('\r'),
            "{source:?}"
        );
    }
}

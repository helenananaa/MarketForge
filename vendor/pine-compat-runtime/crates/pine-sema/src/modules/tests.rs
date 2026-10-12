use super::*;
use crate::analyzer::calls::expr_name;
use crate::source_graph::SourceId;
use pine_syntax::SourceFile;
use std::sync::Arc;

fn parsed_module(id: SourceId, key: Option<&str>, text: &str) -> ModuleInfo {
    let source = Arc::new(SourceFile::new(
        if id == SourceId::root() {
            "root.pine"
        } else {
            "library.pine"
        },
        text,
    ));
    let program = parse_source(&source).program;
    ModuleInfo {
        id,
        source,
        key: key.map(str::to_owned),
        program,
        exports: HashMap::new(),
        private_symbols: HashSet::new(),
        user_types: HashMap::new(),
        methods: HashMap::new(),
        functions: HashMap::new(),
        constants: HashMap::new(),
    }
}

fn qualified_name(parts: &[&str]) -> Expr {
    Expr {
        kind: ExprKind::QualifiedName(parts.iter().map(|part| (*part).to_owned()).collect()),
        span: Span::new(0, 0),
    }
}

#[test]
fn exported_user_type_records_identity_and_fields() {
    let mut module = parsed_module(
        SourceId::library(7),
        Some("user/identity/1"),
        r#"
library("identity")
export type Point
    float x
"#,
    );
    let mut diagnostics = Vec::new();

    collect_library_declarations(&mut module, &mut diagnostics);

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let ExportInfo::UserType {
        identity, fields, ..
    } = module.exports.get("Point").expect("exported UDT")
    else {
        panic!("Point should be a UDT export");
    };
    assert_eq!(identity.source_id, SourceId::library(7));
    assert_eq!(identity.name, "Point");
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "x");
    assert_eq!(fields[0].type_name, "float");
    assert_eq!(
        fields[0].pine_type,
        Some(PineType::new(Qualifier::Series, ValueKind::Float))
    );

    let user_type = module.user_types.get("Point").expect("UDT table entry");
    assert_eq!(user_type.identity, *identity);
    assert_eq!(user_type.fields.len(), 1);
    assert_eq!(user_type.fields[0].name, fields[0].name);
    assert_eq!(user_type.fields[0].type_name, fields[0].type_name);
    assert_eq!(user_type.fields[0].pine_type, fields[0].pine_type);
    assert_eq!(user_type.fields[0].span, fields[0].span);
}

#[test]
fn import_plan_records_alias_qualified_user_type_metadata() {
    let root = parsed_module(
        SourceId::root(),
        None,
        r#"import user/identity/1 as lib
"#,
    );
    let mut library = parsed_module(
        SourceId::library(0),
        Some("user/identity/1"),
        r#"
library("identity")
export type Point
    float x
"#,
    );
    let mut diagnostics = Vec::new();
    collect_library_declarations(&mut library, &mut diagnostics);
    let modules = vec![root, library];
    let library_index = HashMap::from([("user/identity/1".to_owned(), 1)]);

    let plan = build_import_plan(&modules, &library_index, &mut diagnostics);

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let point = plan
        .imported_user_types
        .get("lib.Point")
        .expect("alias-qualified imported UDT");
    assert_eq!(point.identity.source_id, SourceId::library(0));
    assert_eq!(point.identity.name, "Point");
    assert_eq!(point.fields.len(), 1);
    assert_eq!(point.fields[0].name, "x");
    assert_eq!(point.fields[0].type_name, "float");
    assert_eq!(
        point.fields[0].pine_type,
        Some(PineType::new(Qualifier::Series, ValueKind::Float))
    );
    assert_eq!(
        point.span,
        modules[1]
            .user_types
            .get("Point")
            .expect("library UDT metadata")
            .span
    );
}

#[test]
fn import_plan_records_private_user_type_dependencies_for_exported_metadata() {
    let root = parsed_module(
        SourceId::root(),
        None,
        r#"import user/identity/1 as lib
"#,
    );
    let mut library = parsed_module(
        SourceId::library(0),
        Some("user/identity/1"),
        r#"
library("identity")
type Point
    float x
export type Wrapper
    Point nested
"#,
    );
    let mut diagnostics = Vec::new();
    collect_library_declarations(&mut library, &mut diagnostics);
    let modules = vec![root, library];
    let library_index = HashMap::from([("user/identity/1".to_owned(), 1)]);

    let plan = build_import_plan(&modules, &library_index, &mut diagnostics);

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let wrapper = plan
        .imported_user_types
        .get("lib.Wrapper")
        .expect("exported wrapper metadata");
    assert_eq!(wrapper.fields.len(), 1);
    assert_eq!(wrapper.fields[0].name, "nested");
    assert_eq!(wrapper.fields[0].type_name, "Point");
    assert_eq!(wrapper.fields[0].pine_type, None);

    let point = plan
        .imported_user_types
        .get("lib.Point")
        .expect("private dependency metadata");
    assert_eq!(point.identity.name, "Point");
    assert_eq!(point.fields.len(), 1);
    assert_eq!(point.fields[0].name, "x");
    assert_eq!(
        point.fields[0].pine_type,
        Some(PineType::new(Qualifier::Series, ValueKind::Float))
    );
}

#[test]
fn library_method_records_receiver_identity_metadata() {
    let mut module = parsed_module(
        SourceId::library(3),
        Some("user/methods/1"),
        r#"
library("methods")
export type Point
    float x

method shift(Point p, float delta) => p.x + delta
"#,
    );
    let mut diagnostics = Vec::new();

    collect_library_declarations(&mut module, &mut diagnostics);

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let method = module
        .methods
        .get(&("Point".to_owned(), "shift".to_owned()))
        .expect("library method");
    assert_eq!(method.receiver_type_name.as_deref(), Some("Point"));
    assert_eq!(
        method.receiver_identity,
        Some(ModuleUserTypeIdentity {
            source_id: SourceId::library(3),
            name: "Point".to_owned(),
        })
    );
}

#[test]
fn library_method_metadata_allows_same_name_on_different_receivers() {
    let mut module = parsed_module(
        SourceId::library(3),
        Some("user/methods/1"),
        r#"
library("methods")
export type Point
    float x
export type Offset
    int value

method same(Point p) => p
method same(Offset offset) => offset
"#,
    );
    let mut diagnostics = Vec::new();

    collect_library_declarations(&mut module, &mut diagnostics);

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(
        module
            .methods
            .contains_key(&("Point".to_owned(), "same".to_owned()))
    );
    assert!(
        module
            .methods
            .contains_key(&("Offset".to_owned(), "same".to_owned()))
    );
}

#[test]
fn rewrite_context_alias_qualifies_exported_user_type_constructors() {
    let mut module = parsed_module(
        SourceId::library(4),
        Some("user/methods/1"),
        r#"
library("methods")
export type Point
    float x
"#,
    );
    let mut diagnostics = Vec::new();
    collect_library_declarations(&mut module, &mut diagnostics);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let context = rewrite_context_for_module("lib", &module);
    let body = FunctionBody::Expr(qualified_name(&["Point", "new"]));

    let rewritten = rewrite_function_body(&body, &[], &context);

    let FunctionBody::Expr(expr) = rewritten else {
        panic!("expression body expected");
    };
    assert_eq!(expr_name(&expr).as_deref(), Some("lib.Point.new"));
}

#[test]
fn rewrite_context_keeps_shadowed_user_type_constructor_names() {
    let mut context = RewriteContext::default();
    context
        .type_targets
        .insert("Point".to_owned(), "lib.Point".to_owned());
    let body = FunctionBody::Expr(qualified_name(&["Point", "new"]));
    let params = vec!["Point".to_owned()];

    let rewritten = rewrite_function_body(&body, &params, &context);

    let FunctionBody::Expr(expr) = rewritten else {
        panic!("expression body expected");
    };
    assert_eq!(expr_name(&expr).as_deref(), Some("Point.new"));
}

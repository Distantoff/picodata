use crate::backend::sql::ir::PatternWithParams;
use crate::executor::engine::mock::RouterConfigurationMock;
use crate::frontend::sql::ast::AbstractSyntaxTree;
use crate::frontend::Ast;
use crate::ir::transformation::helpers::check_transformation;
use crate::ir::value::Value;
use crate::ir::Plan;
use pretty_assertions::assert_eq;

fn split_columns(plan: &mut Plan) {
    plan.split_columns().unwrap();
}

#[test]
fn split_columns1() {
    let input = r#"SELECT "a" FROM "t" WHERE ("a", 2) = (1, "b")"#;
    let actual_pattern_params = check_transformation(input, vec![], &split_columns);

    assert_eq!(
        actual_pattern_params.params,
        vec![Value::from(1_u64), Value::from(2_u64)]
    );
    insta::assert_snapshot!(
        actual_pattern_params.pattern,
        @r#"SELECT "t"."a" FROM "t" WHERE (("t"."a") = (?)) and ((?) = ("t"."b"))"#
    );
}

#[test]
fn split_columns2() {
    let input = r#"SELECT "a" FROM "t" WHERE "a" = 1"#;
    let actual_pattern_params = check_transformation(input, vec![], &split_columns);

    assert_eq!(actual_pattern_params.params, vec![Value::from(1_u64)]);
    insta::assert_snapshot!(
        actual_pattern_params.pattern,
        @r#"SELECT "t"."a" FROM "t" WHERE ("t"."a") = (?)"#
    );
}

#[test]
fn split_columns3() {
    let query = r#"SELECT "a" FROM "t" WHERE ("a", 2, "b") = (1, "b")"#;

    let metadata = &RouterConfigurationMock::new();
    let mut plan = AbstractSyntaxTree::transform_into_plan(query, metadata).unwrap();
    plan.bind_params(vec![]).unwrap();
    let plan_err = plan.split_columns().unwrap_err();
    assert_eq!(
        format!(
            "{} {} {} {}",
            r#"unexpected number of values:"#,
            r#"left and right rows have different number of columns:"#,
            r#"Row(Row { list: [NodeId { offset: 5, arena_type: Arena96 }, NodeId { offset: 2, arena_type: Arena64 }, NodeId { offset: 6, arena_type: Arena96 }], distribution: None }),"#,
            r#"Row(Row { list: [NodeId { offset: 3, arena_type: Arena64 }, NodeId { offset: 7, arena_type: Arena96 }], distribution: None })"#,
        ),
        format!("{plan_err}")
    );
}

#[test]
fn split_columns4() {
    let input = r#"SELECT "a" FROM "t" WHERE "a" in (1, 2)"#;
    let actual_pattern_params = check_transformation(input, vec![], &split_columns);

    assert_eq!(
        actual_pattern_params.params,
        vec![Value::from(1_u64), Value::from(2_u64)]
    );
    insta::assert_snapshot!(
        actual_pattern_params.pattern,
        @r#"SELECT "t"."a" FROM "t" WHERE ("t"."a") in (?, ?)"#
    );
}

#[test]
fn split_columns5() {
    let input = r#"SELECT "a" FROM "t" WHERE ("a", 2) < (1, "b") and "a" > 2"#;

    let actual_pattern_params = check_transformation(input, vec![], &split_columns);
    assert_eq!(
        actual_pattern_params.params,
        vec![Value::from(1_u64), Value::from(2_u64), Value::from(2_u64)]
    );
    insta::assert_snapshot!(
        actual_pattern_params.pattern,
        @r#"SELECT "t"."a" FROM "t" WHERE ((("t"."a") < (?)) and ((?) < ("t"."b"))) and (("t"."a") > (?))"#
    );
}

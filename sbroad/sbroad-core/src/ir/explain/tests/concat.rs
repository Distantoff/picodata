use super::*;

#[test]
fn concat1_test() {
    explain_check(
        r#"SELECT CAST('1' as string) || 'hello' FROM "t1""#,
        &format!(
            "{}\n{}\n{}\n{}\n{}\n",
            r#"projection (ROW('1'::string) || ROW('hello'::string) -> "col_1")"#,
            r#"    scan "t1""#,
            r#"execution options:"#,
            r#"    sql_vdbe_opcode_max = 45000"#,
            r#"    sql_motion_row_max = 5000"#,
        ),
    );
}

#[test]
fn concat2_test() {
    explain_check(
        r#"SELECT "a" FROM "t1" WHERE CAST('1' as string) || FUNC('hello') || '2' = 42"#,
        &format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n",
            r#"projection ("t1"."a"::string -> "a")"#,
            r#"    selection ROW(ROW(ROW('1'::string) || ROW("func"(('hello'::string))::integer)) || ROW('2'::string)) = ROW(42::unsigned)"#,
            r#"        scan "t1""#,
            r#"execution options:"#,
            r#"    sql_vdbe_opcode_max = 45000"#,
            r#"    sql_motion_row_max = 5000"#,
        ),
    );
}

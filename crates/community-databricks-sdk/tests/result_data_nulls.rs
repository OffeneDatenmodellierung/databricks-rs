//! `ResultData.data_array` keeps null cells (#35): the spec says
//! `array<array<string>>`, but nulls arrive as JSON `null`.

#![cfg(all(feature = "sql", feature = "vectorsearch"))]

use community_databricks_sdk::service::{sql, vectorsearch};

const ROWS: &str = r#"{"data_array": [["a", null], [null, ""]]}"#;

#[test]
fn sql_result_rows_keep_nulls_distinct_from_empty_strings() {
    let r: sql::ResultData = serde_json::from_str(ROWS).unwrap();
    assert_eq!(
        r.data_array,
        vec![
            vec![Some("a".into()), None],
            vec![None, Some(String::new())]
        ]
    );
    // Round trip: nulls are written back as null.
    let back = serde_json::to_value(&r).unwrap();
    assert_eq!(
        back["data_array"],
        serde_json::json!([["a", null], [null, ""]])
    );
}

#[test]
fn vector_search_rows_keep_nulls() {
    let r: vectorsearch::ResultData = serde_json::from_str(ROWS).unwrap();
    assert_eq!(r.data_array[0], vec![Some("a".into()), None]);
}

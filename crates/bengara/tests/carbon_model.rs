//! 日時の列を `Carbon` で持つモデル。DB にはつながず、`Row` と `attributes()` で確かめます。

use bengara::database::Row;
use bengara::{Carbon, Model, Value};

#[derive(Model)]
#[model(table = "events")]
struct Event {
    id: i64,
    starts_at: Carbon,
    ends_at: Option<Carbon>,
    created_at: Carbon,
    updated_at: Option<Carbon>,
}

#[derive(Model)]
#[model(table = "posts")]
struct Post {
    id: i64,
    created_at: String,
    updated_at: Option<String>,
}

fn row(columns: &[&str], values: Vec<Value>) -> Row {
    Row::new(columns.iter().map(|c| c.to_string()).collect(), values)
}

fn value_of<'a>(attributes: &'a [(&'static str, Value)], column: &str) -> &'a Value {
    &attributes
        .iter()
        .find(|(c, _)| *c == column)
        .unwrap_or_else(|| panic!("{column} が無い"))
        .1
}

#[test]
fn carbonの列を読み書きできる() {
    let event = Event::from_row(&row(
        &["id", "starts_at", "ends_at", "created_at", "updated_at"],
        vec![
            Value::Int(1),
            Value::Text("2026-11-20 09:30:00".into()),
            Value::Null,
            // 整数は UNIX 秒として読む。
            Value::Int(0),
            Value::Text("2026-11-20T10:00:00Z".into()),
        ],
    ))
    .unwrap();
    assert_eq!(event.starts_at.to_date_time_string(), "2026-11-20 09:30:00");
    assert_eq!(event.ends_at, None);
    assert_eq!(event.created_at, Carbon::from_timestamp(0));
    assert_eq!(
        event.updated_at.map(|at| at.to_date_time_string()),
        Some("2026-11-20 10:00:00".to_string())
    );

    let attributes = event.attributes();
    assert_eq!(
        value_of(&attributes, "starts_at"),
        &Value::Text("2026-11-20 09:30:00".into())
    );
    assert_eq!(value_of(&attributes, "ends_at"), &Value::Null);
    assert_eq!(
        value_of(&attributes, "updated_at"),
        &Value::Text("2026-11-20 10:00:00".into())
    );
}

#[test]
fn 読めない日時はエラーになる() {
    let result = Event::from_row(&row(
        &["id", "starts_at", "ends_at", "created_at", "updated_at"],
        vec![
            Value::Int(1),
            Value::Text("2026-11-20T09:30:00+09:00".into()),
            Value::Null,
            Value::Int(0),
            Value::Null,
        ],
    ));
    assert!(result.is_err());
}

#[test]
fn 作成日時と更新日時はcarbonの列にも文字列の列にも入る() {
    let before = Carbon::now();

    let mut event = Event {
        id: 0,
        starts_at: Carbon::from_timestamp(0),
        ends_at: None,
        created_at: Carbon::from_timestamp(0),
        updated_at: None,
    };
    event.touch_timestamps(true);
    assert!(event.created_at >= before);
    assert_eq!(event.updated_at, Some(event.created_at));
    // 更新では作成日時を変えない。
    event.created_at = Carbon::from_timestamp(0);
    event.touch_timestamps(false);
    assert_eq!(event.created_at, Carbon::from_timestamp(0));
    assert!(event.updated_at.unwrap() >= before);

    let mut post = Post {
        id: 0,
        created_at: String::new(),
        updated_at: None,
    };
    post.touch_timestamps(true);
    // 文字列に入る形は `now()` と同じ（`YYYY-MM-DD HH:MM:SS`）。
    assert_eq!(post.created_at.len(), 19);
    let created = Carbon::parse(&post.created_at).unwrap();
    assert!(created >= before);
    assert_eq!(post.updated_at.as_deref(), Some(post.created_at.as_str()));
}

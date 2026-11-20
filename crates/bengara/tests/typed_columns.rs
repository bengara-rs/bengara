//! 型付きの列（`#[model(columns)]` と `Column<M, T>`）。
//!
//! DB にはつながず、`to_sql()` の SQL と束縛値で確かめます。
//! コンパイルで止まる書き方は `database/column.rs` の doc テスト（`compile_fail`）にあります。

use std::borrow::Cow;

use bengara::{Carbon, Column, Model, Value, DB};

#[derive(Model)]
#[model(table = "posts", columns)]
struct Post {
    id: i64,
    title: String,
    status: String,
    views: i64,
    published_at: Option<Carbon>,
    #[model(column = "body_text")]
    body: Option<String>,
}

#[derive(Model)]
#[model(table = "users", columns)]
struct User {
    id: i64,
    email: String,
}

#[test]
fn 定数は列名を持つ() {
    assert_eq!(Post::TITLE.name(), "title");
    // `#[model(column = "..")]` で変えた後の名前から作る。
    assert_eq!(Post::BODY_TEXT.name(), "body_text");
    assert_eq!(User::EMAIL.name(), "email");
    // 型はフィールドの型そのまま。
    let _: Column<Post, Option<Carbon>> = Post::PUBLISHED_AT;
    let _: Column<Post, Option<String>> = Post::BODY_TEXT;
}

#[test]
fn 型付きの列で条件と並びを書ける() {
    let (sql, bindings) = Post::query()
        .where_(Post::STATUS, "published")
        .where_op(Post::VIEWS, ">=", 100)
        .where_in(Post::ID, &[1, 2, 3])
        .where_null(Post::BODY_TEXT)
        .where_between(Post::VIEWS, 10u8, 1_000i32)
        .order_by_desc(Post::PUBLISHED_AT)
        .to_sql();
    assert_eq!(
        sql,
        "select * from \"posts\" where \"status\" = ? and \"views\" >= ? \
         and \"id\" in (?, ?, ?) and \"body_text\" is null \
         and \"views\" between ? and ? order by \"published_at\" desc"
    );
    assert_eq!(
        bindings,
        vec![
            Value::Text("published".into()),
            Value::Int(100),
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(10),
            Value::Int(1_000),
        ]
    );
}

#[test]
fn 文字列の列名を混ぜても動く() {
    let (sql, bindings) = Post::query()
        .where_(Post::STATUS, "draft")
        .where_("title", "abc")
        .or_where_op("views", "<", 5)
        .to_sql();
    assert_eq!(
        sql,
        "select * from \"posts\" where \"status\" = ? and \"title\" = ? or \"views\" < ?"
    );
    assert_eq!(
        bindings,
        vec![
            Value::Text("draft".into()),
            Value::Text("abc".into()),
            Value::Int(5)
        ]
    );
}

#[test]
fn クエリビルダにも型付きの列を渡せる() {
    let (sql, bindings) = DB::table("posts")
        .where_(Post::STATUS, "published")
        .where_column(Post::VIEWS, ">", "likes")
        .order_by(Post::TITLE)
        .to_sql();
    assert_eq!(
        sql,
        "select * from \"posts\" where \"status\" = ? and \"views\" > \"likes\" \
         order by \"title\" asc"
    );
    assert_eq!(bindings, vec![Value::Text("published".into())]);
}

#[test]
fn 日時の列には_carbon_を渡せて文字列で束縛する() {
    let (sql, bindings) = Post::query()
        .where_op(Post::PUBLISHED_AT, "<", Carbon::now())
        .to_sql();
    assert_eq!(sql, "select * from \"posts\" where \"published_at\" < ?");
    assert!(matches!(bindings[..], [Value::Text(_)]), "{bindings:?}");

    // `Option<Carbon>` の列には `None` も渡せる（`is null` になる）。
    let (sql, _) = Post::query()
        .where_(Post::PUBLISHED_AT, None::<Carbon>)
        .to_sql();
    assert_eq!(
        sql,
        "select * from \"posts\" where \"published_at\" is null"
    );
}

#[test]
fn 主キーの読み替えは型付きの列でも効く() {
    // 文字列の列名と同じく、主キーの列は値を整数に直す（`ModelQuery::where_`）。
    let (sql, bindings) = Post::query().where_(Post::ID, 12).to_sql();
    assert_eq!(sql, "select * from \"posts\" where \"id\" = ?");
    assert_eq!(bindings, vec![Value::Int(12)]);
}

#[test]
fn 一覧で列を渡すところは_name_を使う() {
    let (sql, _) = Post::query()
        .select(&[Post::ID.name(), Post::TITLE.name()])
        .to_sql();
    assert_eq!(sql, "select \"id\", \"title\" from \"posts\"");
}

/// 値を読む終端メソッドも型付きの列を受ける。DB にはつながないので、組み立てて捨てる。
///
/// ターボフィッシュ（`pluck::<String>(..)`）の書き方がそのまま通ることも確かめる。
#[test]
fn 値を読むメソッドも型付きの列を受ける() {
    let query = Post::query();
    drop(query.pluck::<String>(Post::TITLE));
    drop(query.pluck::<String>("title"));
    drop(query.value::<i64>(Post::VIEWS));
    drop(query.sum::<i64>(Post::VIEWS));
    drop(query.avg::<f64>(Post::VIEWS));
    drop(query.min::<i64>(Post::VIEWS));
    drop(query.max::<i64>("views"));

    let builder = DB::table("posts");
    drop(builder.pluck::<String>(Post::TITLE));
    drop(builder.value::<i64>("views"));
    drop(builder.increment(Post::VIEWS, 1));
    drop(builder.decrement("views", 1));
}

/// 今までの文字列の書き方は、どれもそのまま通る。
#[test]
fn 今までの列名の書き方はそのまま通る() {
    let owned = String::from("title");
    let plain: &str = "title";
    let double: &&str = &plain;
    let cow: Cow<'_, str> = Cow::Borrowed("title");
    let expected = "select * from \"posts\" where \"title\" = ?";

    let forms = [
        Post::query().where_(plain, "a").to_sql().0,
        Post::query().where_(owned.clone(), "a").to_sql().0,
        Post::query().where_(&owned, "a").to_sql().0,
        Post::query().where_(format!("{}le", "tit"), "a").to_sql().0,
        Post::query().where_(owned.as_str(), "a").to_sql().0,
        Post::query().where_(double, "a").to_sql().0,
        Post::query().where_(cow.clone(), "a").to_sql().0,
        Post::query().where_(&cow, "a").to_sql().0,
        DB::table("posts").where_(plain, "a").to_sql().0,
        DB::table("posts").where_(owned.clone(), "a").to_sql().0,
        DB::table("posts").where_(&owned, "a").to_sql().0,
        DB::table("posts")
            .where_(format!("{}le", "tit"), "a")
            .to_sql()
            .0,
        DB::table("posts").where_(owned.as_str(), "a").to_sql().0,
        DB::table("posts").where_(double, "a").to_sql().0,
        DB::table("posts").where_(cow.clone(), "a").to_sql().0,
    ];
    for sql in forms {
        assert_eq!(sql, expected);
    }

    // 文字列の列名には、今までどおり何の値でも渡せる（`Value` も）。
    let (_, bindings) = Post::query()
        .where_("title", Value::Int(1))
        .where_in("views", &[Value::Int(1)])
        .where_in("views", &["1", "2"])
        .where_between(&owned, 1, "z")
        .to_sql();
    assert_eq!(bindings.len(), 6);
}

#[test]
fn 欠けずに広げられる型は渡せる() {
    let small: u32 = 7;
    let text = String::from("x");
    let (_, bindings) = Post::query()
        .where_(Post::VIEWS, small)
        .where_(Post::TITLE, &text)
        .where_(Post::TITLE, text.as_str())
        .where_(Post::BODY_TEXT, "y")
        .where_(Post::BODY_TEXT, Some(String::from("z")))
        .to_sql();
    assert_eq!(
        bindings,
        vec![
            Value::Int(7),
            Value::Text("x".into()),
            Value::Text("x".into()),
            Value::Text("y".into()),
            Value::Text("z".into()),
        ]
    );
}

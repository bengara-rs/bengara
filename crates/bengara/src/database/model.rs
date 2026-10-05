//! モデル。`#[derive(Model)]` が付いた構造体を、表の行として読み書きします。
//!
//! ```ignore
//! #[derive(Model)]
//! #[model(table = "posts")]
//! pub struct Post {
//!     pub id: i64,
//!     pub title: String,
//! }
//!
//! let post = Post::find(1).await?;
//! ```

use std::marker::PhantomData;

use super::query::{Paginator, QueryBuilder};
use super::transaction::Transaction;
use super::value::{FromValue, IntoValue, Row, Value};
use super::Source;
use crate::error::{Error, Result};

/// 表と結びついた構造体。`#[derive(Model)]` が実装します。
///
/// 自分で書くこともできますが、普通は derive に任せます。
pub trait Model: Sized + Send + Sync + 'static {
    /// 表の名前。
    const TABLE: &'static str;
    /// 主キーの列名。
    const PRIMARY_KEY: &'static str;
    /// 列の名前の一覧（主キーを含む）。
    const COLUMNS: &'static [&'static str];

    /// 行から作る。
    fn from_row(row: &Row) -> Result<Self>;

    /// 主キー以外の列と値。
    fn attributes(&self) -> Vec<(&'static str, Value)>;

    /// 主キーの値。
    fn key(&self) -> Value;

    /// 主キーの値を入れる（`insert` の後に呼ばれます）。
    fn set_key(&mut self, value: Value);

    /// `created_at` / `updated_at` を今の時刻にする。
    ///
    /// その名前の `String` のフィールドが無いときは、何もしません。
    fn touch_timestamps(&mut self, creating: bool) {
        let _ = creating;
    }

    /// まだ保存していない行か。主キーが空（`0` / `NULL` / `""`）なら新しい行とみなします。
    fn is_new(&self) -> bool {
        matches!(self.key(), Value::Null | Value::Int(0))
            || self.key() == Value::Text(String::new())
    }

    /// クエリを組み立てる。既定の接続を使います。
    fn query() -> ModelQuery<Self> {
        ModelQuery::new(Source::Default)
    }

    /// トランザクションの中でクエリを組み立てる。
    ///
    /// ```ignore
    /// let tx = DB::begin().await?;
    /// let posts = Post::on(&tx).where_("status", "draft").get().await?;
    /// Post::on(&tx).where_("status", "draft").delete().await?;
    /// tx.commit().await?;
    /// ```
    ///
    /// すでに組み立てたクエリを後から差し替えるときは
    /// [`ModelQuery::using`] を使ってください。
    fn on(tx: &Transaction) -> ModelQuery<Self> {
        ModelQuery::new(tx.source())
    }

    /// 全部取る。
    fn all() -> impl std::future::Future<Output = Result<Vec<Self>>> + Send {
        async { Self::query().get().await }
    }

    /// 主キーで1件取る。
    fn find<V: IntoValue + Send>(
        id: V,
    ) -> impl std::future::Future<Output = Result<Option<Self>>> + Send {
        async move { Self::query().where_(Self::PRIMARY_KEY, id).first().await }
    }

    /// 主キーで1件取る。無ければ 404 のエラー。
    fn find_or_fail<V: IntoValue + Send>(
        id: V,
    ) -> impl std::future::Future<Output = Result<Self>> + Send {
        async move {
            Self::find(id)
                .await?
                .ok_or_else(|| Error::http(404, format!("{} が見つかりません", Self::TABLE)))
        }
    }

    /// 件数を数える。
    fn count() -> impl std::future::Future<Output = Result<i64>> + Send {
        async { Self::query().count().await }
    }

    /// 保存する。新しい行なら足し、そうでなければ更新します。
    ///
    /// **既定の接続を使います。** トランザクションの中で保存したいときは
    /// [`save_using(&tx)`](Model::save_using) を使ってください。
    /// `save()` のままだと、保存はトランザクションの外に出ます。
    /// `rollback()` しても残りますし、`:memory:` のデータベースでは接続が1本なので
    /// 空くのを待って固まります。
    fn save(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        save_to(self, Source::Default)
    }

    /// トランザクションの中で保存する。
    ///
    /// ```ignore
    /// let tx = DB::begin().await?;
    /// let mut post = Post { id: 0, title: "やきそば".into() };
    /// post.save_using(&tx).await?;
    /// tx.commit().await?;
    /// ```
    fn save_using(
        &mut self,
        tx: &Transaction,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        save_to(self, tx.source())
    }

    /// 消す。返るのは消した件数です。
    ///
    /// **既定の接続を使います。** トランザクションの中で消したいときは
    /// [`delete_using(&tx)`](Model::delete_using) を使ってください。
    fn delete(&self) -> impl std::future::Future<Output = Result<u64>> + Send {
        delete_from(self, Source::Default)
    }

    /// トランザクションの中で消す。
    fn delete_using(
        &self,
        tx: &Transaction,
    ) -> impl std::future::Future<Output = Result<u64>> + Send {
        delete_from(self, tx.source())
    }

    /// もう一度読み直す。
    ///
    /// **既定の接続を使います。** トランザクションの中で読み直したいときは
    /// [`fresh_using(&tx)`](Model::fresh_using) を使ってください。
    /// `save_using(&tx)` で保存した内容は、まだ外から見えません。
    fn fresh(&self) -> impl std::future::Future<Output = Result<Option<Self>>> + Send {
        fresh_from(self, Source::Default)
    }

    /// トランザクションの中で読み直す。
    fn fresh_using(
        &self,
        tx: &Transaction,
    ) -> impl std::future::Future<Output = Result<Option<Self>>> + Send {
        fresh_from(self, tx.source())
    }
}

/// `save()` と `save_using()` の中身。投げ先だけが違います。
async fn save_to<T: Model>(model: &mut T, source: Source) -> Result<()> {
    let creating = model.is_new();
    model.touch_timestamps(creating);
    let attributes = model.attributes();
    if attributes.is_empty() {
        return Err(Error::msg(format!(
            "{} には主キー以外の列がありません",
            T::TABLE
        )));
    }
    let query = QueryBuilder::new(source, T::TABLE);
    if creating {
        let id = query.insert_get_id_as(&attributes, T::PRIMARY_KEY).await?;
        model.set_key(Value::Int(id));
    } else {
        query
            .where_(T::PRIMARY_KEY, model.key())
            .update(&attributes)
            .await?;
    }
    Ok(())
}

/// `delete()` と `delete_using()` の中身。
async fn delete_from<T: Model>(model: &T, source: Source) -> Result<u64> {
    if model.is_new() {
        return Ok(0);
    }
    QueryBuilder::new(source, T::TABLE)
        .where_(T::PRIMARY_KEY, model.key())
        .delete()
        .await
}

/// `fresh()` と `fresh_using()` の中身。
async fn fresh_from<T: Model>(model: &T, source: Source) -> Result<Option<T>> {
    ModelQuery::<T>::new(source)
        .where_(T::PRIMARY_KEY, model.key())
        .first()
        .await
}

/// `#[derive(Model)]` が生成するコードから呼ばれます。利用者が直接呼ぶものではありません。
///
/// `insert` が返した主キーを入れられなかったときに知らせます。
/// 黙って既定値で上書きしないためです。
#[doc(hidden)]
pub fn warn_key_not_set(table: &str, column: &str, error: &dyn std::fmt::Display) {
    tracing::warn!("{table}.{column} に insert の戻り値を入れられませんでした: {error}");
}

/// モデルを返すクエリ。中身はクエリビルダと同じです。
pub struct ModelQuery<T> {
    inner: QueryBuilder,
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for ModelQuery<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            marker: PhantomData,
        }
    }
}

impl<T: Model> ModelQuery<T> {
    pub(crate) fn new(source: Source) -> Self {
        Self {
            inner: QueryBuilder::new(source, T::TABLE),
            marker: PhantomData,
        }
    }

    fn map(mut self, f: impl FnOnce(QueryBuilder) -> QueryBuilder) -> Self {
        self.inner = f(self.inner);
        self
    }

    /// トランザクションの中で実行するように差し替える。
    ///
    /// ```ignore
    /// let tx = DB::begin().await?;
    /// let query = Post::query().where_("status", "draft");
    /// let posts = query.clone().using(&tx).get().await?;
    /// query.using(&tx).update(&[("status", "published".into())]).await?;
    /// tx.commit().await?;
    /// ```
    ///
    /// 最初から中で組み立てるなら [`Model::on`] が短く書けます。
    pub fn using(self, tx: &Transaction) -> Self {
        let source = tx.source();
        self.map(|q| q.with_source(source))
    }

    /// 中のクエリビルダを取り出す。生の行が欲しいときの逃げ道です。
    pub fn query_builder(&self) -> QueryBuilder {
        self.inner.clone()
    }

    /// 組み立てた SQL と値の一覧（テストとデバッグ用）。
    pub fn to_sql(&self) -> (String, Vec<Value>) {
        self.inner.to_sql()
    }

    // ---- 条件 ----

    /// 列が値と等しい。
    pub fn where_(self, column: &str, value: impl IntoValue) -> Self {
        self.map(|q| q.where_(column, value))
    }

    /// 列と値を演算子で比べる。
    pub fn where_op(self, column: &str, operator: &str, value: impl IntoValue) -> Self {
        self.map(|q| q.where_op(column, operator, value))
    }

    /// `or` でつないで、列が値と等しい。
    pub fn or_where(self, column: &str, value: impl IntoValue) -> Self {
        self.map(|q| q.or_where(column, value))
    }

    /// `or` でつないで、列と値を演算子で比べる。
    pub fn or_where_op(self, column: &str, operator: &str, value: impl IntoValue) -> Self {
        self.map(|q| q.or_where_op(column, operator, value))
    }

    /// 列が一覧のどれかと等しい。
    pub fn where_in<V: IntoValue + Clone>(self, column: &str, values: &[V]) -> Self {
        self.map(|q| q.where_in(column, values))
    }

    /// 列が一覧のどれとも等しくない。
    pub fn where_not_in<V: IntoValue + Clone>(self, column: &str, values: &[V]) -> Self {
        self.map(|q| q.where_not_in(column, values))
    }

    /// 列が `NULL`。
    pub fn where_null(self, column: &str) -> Self {
        self.map(|q| q.where_null(column))
    }

    /// 列が `NULL` ではない。
    pub fn where_not_null(self, column: &str) -> Self {
        self.map(|q| q.where_not_null(column))
    }

    /// 列が2つの値の間にある。
    pub fn where_between(self, column: &str, low: impl IntoValue, high: impl IntoValue) -> Self {
        self.map(|q| q.where_between(column, low, high))
    }

    /// 列が形に当てはまる（`like`）。
    pub fn where_like(self, column: &str, pattern: impl IntoValue) -> Self {
        self.map(|q| q.where_like(column, pattern))
    }

    /// 列と列を比べる。
    pub fn where_column(self, left: &str, operator: &str, right: &str) -> Self {
        self.map(|q| q.where_column(left, operator, right))
    }

    /// SQL を直接書く。
    pub fn where_raw(self, sql: &str, bindings: &[Value]) -> Self {
        self.map(|q| q.where_raw(sql, bindings))
    }

    /// 括弧でくくった条件の塊を足す。
    pub fn where_group(self, build: impl FnOnce(QueryBuilder) -> QueryBuilder) -> Self {
        self.map(|q| q.where_group(build))
    }

    // ---- 並び・件数 ----

    /// 昇順に並べる。
    pub fn order_by(self, column: &str) -> Self {
        self.map(|q| q.order_by(column))
    }

    /// 降順に並べる。
    pub fn order_by_desc(self, column: &str) -> Self {
        self.map(|q| q.order_by_desc(column))
    }

    /// 新しい順（`created_at` の降順）。
    pub fn latest(self) -> Self {
        self.map(QueryBuilder::latest)
    }

    /// 古い順（`created_at` の昇順）。
    pub fn oldest(self) -> Self {
        self.map(QueryBuilder::oldest)
    }

    /// 指定した列の降順。
    pub fn latest_by(self, column: &str) -> Self {
        self.map(|q| q.latest_by(column))
    }

    /// 指定した列の昇順。
    pub fn oldest_by(self, column: &str) -> Self {
        self.map(|q| q.oldest_by(column))
    }

    /// 取る件数の上限。
    pub fn limit(self, count: u64) -> Self {
        self.map(|q| q.limit(count))
    }

    /// 読み飛ばす件数。
    pub fn offset(self, count: u64) -> Self {
        self.map(|q| q.offset(count))
    }

    /// `limit` と同じ。
    pub fn take(self, count: u64) -> Self {
        self.limit(count)
    }

    /// `offset` と同じ。
    pub fn skip(self, count: u64) -> Self {
        self.offset(count)
    }

    /// ページ番号から件数を決める。
    pub fn for_page(self, page: u64, per_page: u64) -> Self {
        self.map(|q| q.for_page(page, per_page))
    }

    // ---- 実行 ----

    /// 取る。
    pub async fn get(&self) -> Result<Vec<T>> {
        let rows = self.inner.get().await?;
        rows.iter().map(T::from_row).collect()
    }

    /// 最初の1件。
    pub async fn first(&self) -> Result<Option<T>> {
        match self.inner.first().await? {
            Some(row) => T::from_row(&row).map(Some),
            None => Ok(None),
        }
    }

    /// 最初の1件。無ければ 404 のエラー。
    pub async fn first_or_fail(&self) -> Result<T> {
        self.first()
            .await?
            .ok_or_else(|| Error::http(404, format!("{} が見つかりません", T::TABLE)))
    }

    /// 件数。
    pub async fn count(&self) -> Result<i64> {
        self.inner.count().await
    }

    /// 1件でもあるか。
    pub async fn exists(&self) -> Result<bool> {
        self.inner.exists().await
    }

    /// ある列だけを一覧で取る。
    pub async fn pluck<V: FromValue>(&self, column: &str) -> Result<Vec<V>> {
        self.inner.pluck(column).await
    }

    /// 最初の行の、ある列の値。
    pub async fn value<V: FromValue>(&self, column: &str) -> Result<Option<V>> {
        self.inner.value(column).await
    }

    /// 条件に当てはまる行を更新する。
    pub async fn update(&self, values: &[(&str, Value)]) -> Result<u64> {
        self.inner.update(values).await
    }

    /// 条件に当てはまる行を消す。
    pub async fn delete(&self) -> Result<u64> {
        self.inner.delete().await
    }

    /// ページ分けして取る。
    pub async fn paginate(&self, per_page: u64, page: u64) -> Result<Paginator<T>> {
        let page = self.inner.paginate(per_page, page).await?;
        let data: Result<Vec<T>> = page.data.iter().map(T::from_row).collect();
        let data = data?;
        Ok(page.map_data(data))
    }
}

impl<T: Model> std::fmt::Debug for ModelQuery<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (sql, bindings) = self.to_sql();
        f.debug_struct("ModelQuery")
            .field("table", &T::TABLE)
            .field("sql", &sql)
            .field("bindings", &bindings.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::backend::{Backend, DbFuture, RawTx};
    use super::super::grammar::Driver;
    use super::super::value::Affected;
    use super::*;

    /// 流れてきた SQL を覚えるだけの接続。
    ///
    /// 既定の接続（プール）とトランザクションを別に数えるので、
    /// どちらへ流れたかが分かります。
    #[derive(Default)]
    struct Spy {
        pool: Arc<Mutex<Vec<String>>>,
        tx: Arc<Mutex<Vec<String>>>,
    }

    fn remember(log: &Mutex<Vec<String>>, sql: &str) {
        log.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(sql.to_string());
    }

    fn joined(log: &Mutex<Vec<String>>) -> String {
        log.lock().unwrap_or_else(|e| e.into_inner()).join("\n")
    }

    impl Backend for Spy {
        fn driver(&self) -> Driver {
            Driver::Sqlite
        }

        fn fetch_all<'a>(&'a self, sql: &'a str, _b: &'a [Value]) -> DbFuture<'a, Vec<Row>> {
            remember(&self.pool, sql);
            Box::pin(async { Ok(Vec::new()) })
        }

        fn execute<'a>(&'a self, sql: &'a str, _b: &'a [Value]) -> DbFuture<'a, Affected> {
            remember(&self.pool, sql);
            Box::pin(async { Ok(affected()) })
        }

        fn begin(&self) -> DbFuture<'_, Box<dyn RawTx>> {
            let log = Arc::clone(&self.tx);
            Box::pin(async move { Ok(Box::new(SpyTx { log }) as Box<dyn RawTx>) })
        }

        fn table_names(&self) -> DbFuture<'_, Vec<String>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    struct SpyTx {
        log: Arc<Mutex<Vec<String>>>,
    }

    impl RawTx for SpyTx {
        fn fetch_all<'a>(&'a mut self, sql: &'a str, _b: &'a [Value]) -> DbFuture<'a, Vec<Row>> {
            remember(&self.log, sql);
            Box::pin(async { Ok(Vec::new()) })
        }

        fn execute<'a>(&'a mut self, sql: &'a str, _b: &'a [Value]) -> DbFuture<'a, Affected> {
            remember(&self.log, sql);
            Box::pin(async { Ok(affected()) })
        }

        fn commit(self: Box<Self>) -> DbFuture<'static, ()> {
            Box::pin(async { Ok(()) })
        }

        fn rollback(self: Box<Self>) -> DbFuture<'static, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    fn affected() -> Affected {
        Affected {
            rows: 1,
            last_insert_id: Some(7),
        }
    }

    /// 覚える接続と、そこから始めたトランザクションを用意する。
    async fn spy() -> (Arc<Spy>, Transaction) {
        let found = Arc::new(Spy::default());
        let backend: Arc<dyn Backend> = found.clone();
        let tx = Transaction::start(backend)
            .await
            .expect("トランザクションが始まる");
        (found, tx)
    }

    struct Post {
        id: i64,
        title: String,
    }

    impl Model for Post {
        const TABLE: &'static str = "posts";
        const PRIMARY_KEY: &'static str = "id";
        const COLUMNS: &'static [&'static str] = &["id", "title"];

        fn from_row(row: &Row) -> Result<Self> {
            Ok(Self {
                id: row.get("id")?,
                title: row.get("title")?,
            })
        }

        fn attributes(&self) -> Vec<(&'static str, Value)> {
            vec![("title", Value::Text(self.title.clone()))]
        }

        fn key(&self) -> Value {
            Value::Int(self.id)
        }

        fn set_key(&mut self, value: Value) {
            self.id = i64::from_value(&value).unwrap_or(0);
        }
    }

    #[test]
    fn クエリは表の名前から始まる() {
        let (sql, bindings) = Post::query().where_("title", "やきそば").latest().to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" where \"title\" = ? order by \"created_at\" desc"
        );
        assert_eq!(bindings, vec![Value::Text("やきそば".into())]);
    }

    #[test]
    fn 行から作れる() {
        let row = Row::new(
            vec!["id".into(), "title".into()],
            vec![Value::Int(3), Value::Text("やきそば".into())],
        );
        let post = Post::from_row(&row).unwrap();
        assert_eq!(post.id, 3);
        assert_eq!(post.title, "やきそば");
        assert!(!post.is_new());
    }

    #[test]
    fn 主キーが空なら新しい行とみなす() {
        let post = Post {
            id: 0,
            title: String::new(),
        };
        assert!(post.is_new());
    }

    #[test]
    fn デバッグ表示にsqlが出る() {
        let text = format!("{:?}", Post::query().where_("id", 1));
        assert!(text.contains("posts"));
    }

    #[tokio::test]
    async fn save_usingはトランザクションの中に書く() {
        let (found, tx) = spy().await;
        let mut post = Post {
            id: 0,
            title: "やきそば".into(),
        };
        post.save_using(&tx).await.expect("保存できる");

        assert_eq!(post.id, 7, "insert が返した ID が入る");
        assert!(
            joined(&found.pool).is_empty(),
            "既定の接続には流れない: {}",
            joined(&found.pool)
        );
        let sent = joined(&found.tx);
        assert!(sent.contains("insert into \"posts\""), "{sent}");
    }

    #[tokio::test]
    async fn save_usingは更新もトランザクションの中で流す() {
        let (found, tx) = spy().await;
        let mut post = Post {
            id: 3,
            title: "やきそば".into(),
        };
        post.save_using(&tx).await.expect("更新できる");
        assert!(joined(&found.pool).is_empty());
        let sent = joined(&found.tx);
        assert!(
            sent.contains("update \"posts\" set \"title\" = ?"),
            "{sent}"
        );
        assert!(sent.contains("where \"id\" = ?"), "{sent}");
    }

    #[tokio::test]
    async fn delete_usingとfresh_usingもトランザクションの中で流す() {
        let (found, tx) = spy().await;
        let post = Post {
            id: 3,
            title: "やきそば".into(),
        };
        assert_eq!(post.delete_using(&tx).await.expect("消せる"), 1);
        assert!(post.fresh_using(&tx).await.expect("読み直せる").is_none());
        assert!(joined(&found.pool).is_empty(), "既定の接続には流れない");
        let sent = joined(&found.tx);
        assert!(
            sent.contains("delete from \"posts\" where \"id\" = ?"),
            "{sent}"
        );
        assert!(
            sent.contains("select * from \"posts\" where \"id\" = ?"),
            "{sent}"
        );
    }
}

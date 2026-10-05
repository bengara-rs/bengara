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

    /// クエリを組み立てる。
    fn query() -> ModelQuery<Self> {
        ModelQuery::new(Source::Default)
    }

    /// トランザクションの中でクエリを組み立てる。
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
    fn save(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        async move {
            let creating = self.is_new();
            self.touch_timestamps(creating);
            let attributes = self.attributes();
            if attributes.is_empty() {
                return Err(Error::msg(format!(
                    "{} には主キー以外の列がありません",
                    Self::TABLE
                )));
            }
            if creating {
                let id = super::DB::table(Self::TABLE)
                    .insert_get_id_as(&attributes, Self::PRIMARY_KEY)
                    .await?;
                self.set_key(Value::Int(id));
            } else {
                super::DB::table(Self::TABLE)
                    .where_(Self::PRIMARY_KEY, self.key())
                    .update(&attributes)
                    .await?;
            }
            Ok(())
        }
    }

    /// 消す。返るのは消した件数です。
    fn delete(&self) -> impl std::future::Future<Output = Result<u64>> + Send {
        async move {
            if self.is_new() {
                return Ok(0);
            }
            super::DB::table(Self::TABLE)
                .where_(Self::PRIMARY_KEY, self.key())
                .delete()
                .await
        }
    }

    /// もう一度読み直す。
    fn fresh(&self) -> impl std::future::Future<Output = Result<Option<Self>>> + Send {
        async move {
            Self::query()
                .where_(Self::PRIMARY_KEY, self.key())
                .first()
                .await
        }
    }
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
    use super::*;

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
}

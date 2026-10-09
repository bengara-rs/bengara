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
    /// **当てはまる行が無いときはエラーです。** 更新のつもりで呼んで1件も当たらない
    /// のは、消えた行・推測した主キー・並行削除のどれかです。
    /// 以前は警告だけで `Ok` を返していたので、書き込みが静かに失われていました。
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
        if model.is_new() {
            // `set_key` が値を入れられなかった（主キーの型に収まらないなど）。
            // 主キーが空のままなので、もう一度 `save()` すると2行目が入ります。
            // `u8` を主キーにして 256 行目を入れると、以前はそこで静かに重複していました。
            return Err(Error::msg(format!(
                "採番された主キー {id} を {}.{} に入れられませんでした（入った値: `{}`）。\
                 主キーの型を、採番された値が収まる型（`i64` など）にしてください",
                std::any::type_name::<T>(),
                T::PRIMARY_KEY,
                model.key()
            )));
        }
    } else {
        let key = model.key();
        let affected = query
            .where_(T::PRIMARY_KEY, key.clone())
            .update(&attributes)
            .await?;
        if affected == 0 {
            // 当てはまる行が無かった。消えた行・推測した主キー・並行削除のどれか。
            // 以前は警告だけで `Ok` を返していたので、書き込みが静かに失われていました。
            return Err(Error::msg(format!(
                "{}.{} = {key} の行が無いので保存できませんでした。\
                 消えた行を保存しようとしていないか、主キーの値を確かめてください",
                T::TABLE,
                T::PRIMARY_KEY
            )));
        }
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
///
/// 入らなかったことは `save()` が後から見つけてエラーにします
/// （主キーが空のままだと、次の `save()` が2行目を入れてしまうため）。
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
    ///
    /// **ここに無い操作は、これで降りてください。** 戻るのは `QueryBuilder` なので、
    /// モデルの型は外れます。
    pub fn query_builder(&self) -> QueryBuilder {
        self.inner.clone()
    }

    /// 組み立てた SQL と値の一覧（テストとデバッグ用）。
    pub fn to_sql(&self) -> (String, Vec<Value>) {
        self.inner.to_sql()
    }

    // ---- 取る列 ----

    /// 取る列を決める。`QueryBuilder::select` と同じく列名を検査しません。
    pub fn select(self, columns: &[&str]) -> Self {
        self.map(|q| q.select(columns))
    }

    /// 取る列を足す。列名を検査しません。
    pub fn add_select(self, column: &str) -> Self {
        self.map(|q| q.add_select(column))
    }

    /// 重なりを捨てる。
    pub fn distinct(self) -> Self {
        self.map(QueryBuilder::distinct)
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

    /// `or` でつないで、列が一覧のどれかと等しい。
    pub fn or_where_in<V: IntoValue + Clone>(self, column: &str, values: &[V]) -> Self {
        self.map(|q| q.or_where_in(column, values))
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

    /// 列が2つの値の間に無い。
    pub fn where_not_between(
        self,
        column: &str,
        low: impl IntoValue,
        high: impl IntoValue,
    ) -> Self {
        self.map(|q| q.where_not_between(column, low, high))
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

    /// `or` でつないで、括弧でくくった条件の塊を足す。
    pub fn or_where_group(self, build: impl FnOnce(QueryBuilder) -> QueryBuilder) -> Self {
        self.map(|q| q.or_where_group(build))
    }

    // ---- 束ね ----

    /// 列で束ねる。
    pub fn group_by(self, columns: &[&str]) -> Self {
        self.map(|q| q.group_by(columns))
    }

    /// 束ねた結果を絞る。`group_by` と一緒に使ってください。
    ///
    /// **渡せるのは実在の列だけです。** `select` に書いた別名は渡せません
    /// （[`QueryBuilder::having_op`] を参照）。集計の結果で絞るときは
    /// `having_raw` を使ってください。
    pub fn having_op(self, column: &str, operator: &str, value: impl IntoValue) -> Self {
        self.map(|q| q.having_op(column, operator, value))
    }

    /// 束ねた結果を SQL で絞る。
    pub fn having_raw(self, sql: &str, bindings: &[Value]) -> Self {
        self.map(|q| q.having_raw(sql, bindings))
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

    /// 並べ方を SQL で直接書く。**外から来た文字列を渡さないでください。**
    pub fn order_by_raw(self, sql: &str) -> Self {
        self.map(|q| q.order_by_raw(sql))
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

    /// 1件も無いか。
    pub async fn doesnt_exist(&self) -> Result<bool> {
        self.inner.doesnt_exist().await
    }

    /// 合計。
    pub async fn sum<V: FromValue>(&self, column: &str) -> Result<Option<V>> {
        self.inner.sum(column).await
    }

    /// 平均。
    pub async fn avg<V: FromValue>(&self, column: &str) -> Result<Option<V>> {
        self.inner.avg(column).await
    }

    /// 最小。
    pub async fn min<V: FromValue>(&self, column: &str) -> Result<Option<V>> {
        self.inner.min(column).await
    }

    /// 最大。
    pub async fn max<V: FromValue>(&self, column: &str) -> Result<Option<V>> {
        self.inner.max(column).await
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

    /// 決まった結果だけを返す接続。件数と採番された ID を自由に決められます。
    struct Fixed(Affected);

    impl Backend for Fixed {
        fn driver(&self) -> Driver {
            Driver::Sqlite
        }

        fn fetch_all<'a>(&'a self, _sql: &'a str, _b: &'a [Value]) -> DbFuture<'a, Vec<Row>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn execute<'a>(&'a self, _sql: &'a str, _b: &'a [Value]) -> DbFuture<'a, Affected> {
            let affected = self.0;
            Box::pin(async move { Ok(affected) })
        }

        fn begin(&self) -> DbFuture<'_, Box<dyn RawTx>> {
            let affected = self.0;
            Box::pin(async move { Ok(Box::new(FixedTx(affected)) as Box<dyn RawTx>) })
        }

        fn table_names(&self) -> DbFuture<'_, Vec<String>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    struct FixedTx(Affected);

    impl RawTx for FixedTx {
        fn fetch_all<'a>(&'a mut self, _sql: &'a str, _b: &'a [Value]) -> DbFuture<'a, Vec<Row>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn execute<'a>(&'a mut self, _sql: &'a str, _b: &'a [Value]) -> DbFuture<'a, Affected> {
            let affected = self.0;
            Box::pin(async move { Ok(affected) })
        }

        fn commit(self: Box<Self>) -> DbFuture<'static, ()> {
            Box::pin(async { Ok(()) })
        }

        fn rollback(self: Box<Self>) -> DbFuture<'static, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    /// 決まった結果を返す接続から、トランザクションを始める。
    async fn fixed(affected: Affected) -> Transaction {
        let backend: Arc<dyn Backend> = Arc::new(Fixed(affected));
        Transaction::start(backend)
            .await
            .expect("トランザクションが始まる")
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

    /// 主キーが1バイトのモデル。採番された値が入らない場合を試します。
    struct Tiny {
        id: u8,
        title: String,
    }

    impl Model for Tiny {
        const TABLE: &'static str = "tiny";
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
            Value::Int(i64::from(self.id))
        }

        fn set_key(&mut self, value: Value) {
            // `#[derive(Model)]` が生成するのと同じ形。読めないときは値を変えない。
            if let Ok(id) = u8::from_value(&value) {
                self.id = id;
            }
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
    fn 委譲した操作でもモデルの型が外れない() {
        // 戻るのが ModelQuery<Post> のままなので、最後まで型が付いて回る。
        let (sql, bindings) = Post::query()
            .select(&["id", "title"])
            .distinct()
            .where_not_between("id", 1, 3)
            .or_where_in("id", &[7, 8])
            .or_where_group(|q| q.where_("title", "x"))
            .group_by(&["title"])
            .having_op("id", ">", 0)
            .order_by_raw("title asc")
            .to_sql();
        assert_eq!(
            sql,
            "select distinct \"id\", \"title\" from \"posts\" \
             where \"id\" not between ? and ? or \"id\" in (?, ?) or (\"title\" = ?) \
             group by \"title\" having \"id\" > ? order by title asc"
        );
        assert_eq!(bindings.len(), 6);
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

    #[tokio::test]
    async fn 行が無い更新はエラーになる() {
        let tx = fixed(Affected {
            rows: 0,
            last_insert_id: None,
        })
        .await;
        let mut post = Post {
            id: 3,
            title: "やきそば".into(),
        };
        // 以前は警告だけで Ok を返していたので、書き込みが静かに失われていた。
        let message = post
            .save_using(&tx)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("posts.id = 3"), "{message}");
        assert!(message.contains("保存できませんでした"), "{message}");
    }

    #[tokio::test]
    async fn 採番された主キーが入らないときはエラーになる() {
        let tx = fixed(Affected {
            rows: 1,
            last_insert_id: Some(300),
        })
        .await;
        let mut tiny = Tiny {
            id: 0,
            title: "やきそば".into(),
        };
        // 以前は警告だけで Ok を返していたので、もう一度 save() すると2行目が入っていた。
        let message = tiny
            .save_using(&tx)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("300"), "{message}");
        assert!(message.contains("Tiny"), "{message}");
        assert!(tiny.is_new(), "主キーは入っていないまま");
    }

    #[tokio::test]
    async fn 入る主キーならこれまでどおり保存できる() {
        let tx = fixed(Affected {
            rows: 1,
            last_insert_id: Some(7),
        })
        .await;
        let mut tiny = Tiny {
            id: 0,
            title: "やきそば".into(),
        };
        tiny.save_using(&tx).await.expect("保存できる");
        assert_eq!(tiny.id, 7);
        assert!(!tiny.is_new());
    }
}

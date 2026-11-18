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

use super::grammar::Driver;
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

    /// 主キーが整数の列か。`#[derive(Model)]` は整数の主キー専用なので、既定は真です。
    ///
    /// 真のとき、`find` などに渡された**整数に読める文字列**は整数として束縛します。
    /// パス引数やセッションの ID は文字列で届くためです。PostgreSQL は
    /// `bigint = text` を断るので、文字列のまま渡すと `operator does not exist` に
    /// なります（SQLite と MySQL は暗黙に変換するので気づけません）。
    ///
    /// **真のとき、整数に読めない値では探しません。** `find("abc")` や
    /// `/posts/abc` への `req.model::<Post>("post")` は「無い」を返します。
    /// そのまま比べると、PostgreSQL だけ `bigint = text` の SQL エラー（500）に
    /// なるためです。
    ///
    /// 文字列の主キーを自分で実装するときは **必ず** `false` にしてください。
    /// `"123"` のような値をそのまま文字列として探せるようになります。
    /// 真のままだと、`"a1"` のような主キーは1件も見つかりません。
    const INTEGER_KEY: bool = true;

    /// 主キーの列に渡す値を、主キーの型に合わせて直す。
    ///
    /// `INTEGER_KEY` が真なら、整数に読める文字列を `Value::Int` にします。
    /// それ以外はそのまま返します。
    fn key_value(value: impl IntoValue) -> Value {
        let value = value.into_value();
        if Self::INTEGER_KEY {
            super::normalize_key(value)
        } else {
            value
        }
    }

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
    /// その**列名**のフィールドが無いときは、何もしません。
    /// 入れる値は `now()` の文字列です。`#[derive(Model)]` は `.into()` で入れるので、
    /// `String` から `Into` で変えられる型のフィールドなら動きます。
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
    ///
    /// 整数に読める文字列（パス引数など）は整数として探します（[`Model::key_value`]）。
    ///
    /// **`NULL` と、整数の主キーに整数として読めない値は、問い合わせずに `None` です**
    /// （[`Model::INTEGER_KEY`]）。
    fn find<V: IntoValue + Send>(
        id: V,
    ) -> impl std::future::Future<Output = Result<Option<Self>>> + Send {
        let id = id.into_value();
        async move { find_by_key(&Self::query(), id).await }
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
    /// # 更新のときは**全列**を書き戻します
    ///
    /// 変えたのが1つの項目だけでも、構造体が持っている値を全部書きます
    /// （`created_at` だけは作ったときの値を保ちます）。
    /// そのため、**読んでから保存するまでの間に他の人が動かした列は巻き戻ります。**
    ///
    /// ```ignore
    /// // 題名だけ変えたいのに、読んだときの views も一緒に書き戻る。
    /// let mut post = Post::find_or_fail(id).await?;
    /// post.title = "新しい題名".into();
    /// post.save().await?;                       // ← views が戻る
    ///
    /// // 動く列がある表では、変える列だけを指定する。
    /// post.save_only(&["title"]).await?;        // ← title と updated_at だけ
    /// ```
    ///
    /// 変えたい項目を数えられるなら、[`save_only`](Model::save_only) か
    /// クエリビルダの `update` を使ってください。
    /// どの列が「他の人も動かす列」かは枠組みから分からないので、ここでは
    /// 何が変わったかを追いません。
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

    /// 指定した列だけを書き戻す。返るのは当てはまった行の数です。
    ///
    /// ```ignore
    /// let mut post = Post::find_or_fail(id).await?;
    /// post.title = "新しい題名".into();
    /// if post.save_only(&["title"]).await? == 0 {
    ///     // 読んだ後で行が消えた。
    ///     return Err(Error::http(404, "見つかりません"));
    /// }
    /// ```
    ///
    /// 他の人も動かす列（閲覧数など）がある表で、`save()` の代わりに使います。
    /// 書くのは指定した列と `updated_at` だけなので、ほかの列は巻き戻りません。
    ///
    /// | 項目               | `save()`                         | `save_only(&[..])`                  |
    /// |--------------------|----------------------------------|-------------------------------------|
    /// | 書く列             | 主キーと `created_at` 以外の全部 | 指定した列と `updated_at`（あれば） |
    /// | 新しい行           | 足す                             | **エラー**（足しません）            |
    /// | 当てはまる行が無い | **エラー**                       | **`Ok(0)`**（エラーにしません）     |
    /// | 返るもの           | `()`                             | 当てはまった行の数（`u64`）         |
    ///
    /// - **`updated_at` は自動で足します。** その列がモデルにあれば、今の時刻にして
    ///   一緒に書きます（`save()` と同じ [`Model::touch_timestamps`]）。
    /// - 行が無いときを `Ok(0)` にするのは、呼ぶ側で 404 などに分けられるようにするためです。
    /// - 次のときはエラーです。何も書きません。
    ///   - 列を1つも指定していない
    ///   - モデルに無い列（`COLUMNS` に無い名前）を指定した
    ///   - 主キーの列を指定した
    ///   - まだ保存していない行（[`Model::is_new`] が真）
    ///
    /// **既定の接続を使います。** トランザクションの中で書くときは
    /// [`save_only_using(&tx, ..)`](Model::save_only_using) を使ってください。
    fn save_only(
        &mut self,
        columns: &[&str],
    ) -> impl std::future::Future<Output = Result<u64>> + Send {
        save_only_to(self, columns, Source::Default)
    }

    /// トランザクションの中で、指定した列だけを書き戻す。
    ///
    /// 決まりは [`save_only`](Model::save_only) と同じです。
    fn save_only_using(
        &mut self,
        tx: &Transaction,
        columns: &[&str],
    ) -> impl std::future::Future<Output = Result<u64>> + Send {
        save_only_to(self, columns, tx.source())
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
        // 採番が返らなかった（MySQL の `auto_increment` の無い表）ときは、
        // `insert_get_id_as` が「行は入っています」と案内するエラーを返します。
        model.set_key(Value::Int(id));
        if model.is_new() {
            // 主キーが空のままなので、もう一度 `save()` すると2行目が入ります。
            // `set_key` が値を入れられなかった（主キーの型に収まらない）。
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
        // 探しても無いと分かっている主キー（整数の主キーに文字列など）は、
        // 問い合わせずに「行が無い」にする。PostgreSQL では SQL エラーになる形。
        let Some(key) = lookup_key::<T>(model.key(), query.driver()) else {
            query.check()?;
            return Err(missing_row::<T>(&T::key_value(model.key())));
        };
        // `created_at` は作ったときの値を保つ。全列を書き戻す作りなので、
        // 読み直さずに作った構造体（`created_at` が空）を保存すると、
        // 以前は `created_at = ''` で上書きされていた。
        let updating: Vec<(&'static str, Value)> = attributes
            .iter()
            .filter(|(name, _)| *name != "created_at")
            .cloned()
            .collect();
        // `created_at` しか無い表では、外すと更新する列が無くなる。そのときは全部書く。
        let updating = if updating.is_empty() {
            attributes
        } else {
            updating
        };
        let affected = query
            .where_(T::PRIMARY_KEY, key.clone())
            .update(&updating)
            .await?;
        if affected == 0 {
            // 当てはまる行が無かった。消えた行・推測した主キー・並行削除のどれか。
            // 以前は警告だけで `Ok` を返していたので、書き込みが静かに失われていました。
            return Err(missing_row::<T>(&key));
        }
    }
    Ok(())
}

/// `save_only()` と `save_only_using()` の中身。投げ先だけが違います。
///
/// 列の検査は、時刻を入れる前・問い合わせる前に済ませます。
/// 断るときに構造体を書き換えないためです。
async fn save_only_to<T: Model>(model: &mut T, columns: &[&str], source: Source) -> Result<u64> {
    if model.is_new() {
        return Err(Error::msg(format!(
            "{} の save_only() は保存済みの行にだけ使えます。\
             新しい行は save() で入れてください",
            T::TABLE
        )));
    }
    if columns.is_empty() {
        return Err(Error::msg(format!(
            "{} の save_only() に書く列を1つ以上指定してください",
            T::TABLE
        )));
    }
    for column in columns {
        if *column == T::PRIMARY_KEY {
            return Err(Error::msg(format!(
                "{}.{} は主キーなので save_only() では書けません",
                T::TABLE,
                column
            )));
        }
        if !T::COLUMNS.contains(column) {
            return Err(Error::msg(format!(
                "{} に `{column}` という列はありません（ある列: {}）",
                T::TABLE,
                T::COLUMNS.join(", ")
            )));
        }
    }
    // `COLUMNS` にあって `attributes()` に無い列（手で実装したモデルの食い違い）。
    // 黙って書かずに済ませると、変えたつもりの値が残りません。
    let known = model.attributes();
    if let Some(column) = columns
        .iter()
        .find(|c| !known.iter().any(|(name, _)| name == *c))
    {
        return Err(Error::msg(format!(
            "{}.{column} は attributes() に無いので書けません",
            T::TABLE
        )));
    }
    let query = QueryBuilder::new(source, T::TABLE);
    // 探しても無いと分かっている主キーは、問い合わせずに「行が無い」（0 件）にする。
    let Some(key) = lookup_key::<T>(model.key(), query.driver()) else {
        query.check()?;
        return Ok(0);
    };
    model.touch_timestamps(false);
    let updating: Vec<(&'static str, Value)> = model
        .attributes()
        .into_iter()
        .filter(|(name, _)| columns.contains(name) || *name == "updated_at")
        .collect();
    query.where_(T::PRIMARY_KEY, key).update(&updating).await
}

/// 更新のつもりで `save()` して、当てはまる行が無かったときのエラー。
///
/// **主キーを自分で決める行を `save()` で入れようとした場合もここに来ます。**
/// 主キーに値が入っていると「保存済み」とみなして `update` を流すためです。
/// 以前は「値を入れてから `save()`」と案内していましたが、その通りにすると
/// このエラーになるので、入れ方（`DB::table(..).insert(..)`）を添えます。
fn missing_row<T: Model>(key: &Value) -> Error {
    Error::msg(format!(
        "{}.{} = {key} の行が無いので保存できませんでした。\
         消えた行を保存しようとしていないか、主キーの値を確かめてください。\
         主キーを自分で決める行を新しく入れるときは、`DB::table(..).insert(..)` を\
         使ってください（`save()` が新しく入れるのは、主キーが空の行だけです）",
        T::TABLE,
        T::PRIMARY_KEY
    ))
}

/// 主キーで探すときに渡す値を決める。**探しても無いと分かっているときは `None`。**
///
/// | 値                                       | 返すもの                  |
/// |------------------------------------------|---------------------------|
/// | `NULL`                                   | `None`                    |
/// | 整数の主キーに、整数として読めない文字列 | `None`                    |
/// | それ以外                                 | [`Model::key_value`] の値 |
///
/// `NULL` を外すのは、`where_` が `NULL` を `is null` に読み替えるためです。
/// そのまま渡すと「主キーが NULL の行」を探してしまいます。
fn lookup_key<T: Model>(value: impl IntoValue, driver: Driver) -> Option<Value> {
    let value = T::key_value(value);
    if value.is_null() || (T::INTEGER_KEY && never_matches_integer_key(&value, driver)) {
        return None;
    }
    Some(value)
}

/// 整数の主キーと比べても、当たる行が無いと分かっている値か。
///
/// 整数として読めない文字列（`"abc"`）がこれです。そのまま束縛すると、
/// PostgreSQL は `bigint = text` を断って SQL エラー（500）になります。
/// MySQL は文字列を `0` に読み替えるので、`id = 0` の行に当たってしまいます。
/// どちらも「無い」が正しい答えです。
///
/// `i64` を超える数字だけの文字列（`u64` の大きな値）は、MySQL でだけ通します。
/// MySQL の `id()` は `bigint unsigned` なので、その値の行がありえます。
/// SQLite と PostgreSQL の整数は `i64` までなので、当たる行がありません。
fn never_matches_integer_key(value: &Value, driver: Driver) -> bool {
    let Value::Text(text) = value else {
        return false;
    };
    let text = text.trim();
    if text.parse::<i64>().is_ok() {
        return false;
    }
    !(driver == Driver::MySql && text.parse::<u64>().is_ok())
}

/// `Model::find` / `ModelQuery::find` / `fresh` の中身。
///
/// 探しても無いと分かっている値のときは、問い合わせずに `None` を返します。
/// 組み立てと接続の間違いは、そのときも知らせます。
async fn find_by_key<T: Model>(query: &ModelQuery<T>, id: Value) -> Result<Option<T>> {
    let Some(key) = lookup_key::<T>(id, query.inner.driver()) else {
        query.inner.check()?;
        return Ok(None);
    };
    let row = query
        .inner
        .clone()
        .where_(T::PRIMARY_KEY, key)
        .first()
        .await?;
    match row {
        Some(row) => T::from_row(&row).map(Some),
        None => Ok(None),
    }
}

/// `delete()` と `delete_using()` の中身。
async fn delete_from<T: Model>(model: &T, source: Source) -> Result<u64> {
    if model.is_new() {
        return Ok(0);
    }
    let query = QueryBuilder::new(source, T::TABLE);
    // 探しても無いと分かっている主キーは、問い合わせずに 0 件にする。
    let Some(key) = lookup_key::<T>(model.key(), query.driver()) else {
        query.check()?;
        return Ok(0);
    };
    query.where_(T::PRIMARY_KEY, key).delete().await
}

/// `fresh()` と `fresh_using()` の中身。
async fn fresh_from<T: Model>(model: &T, source: Source) -> Result<Option<T>> {
    find_by_key(&ModelQuery::<T>::new(source), model.key()).await
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

    /// 列が値と等しい。値が `NULL`（`None`）のときは `is null` になります。
    ///
    /// **整数の主キーの列には、値を主キーの型に合わせて渡します**（[`Model::key_value`]）。
    ///
    /// | 渡した値                       | 置く条件                       |
    /// |--------------------------------|--------------------------------|
    /// | 整数に読める文字列（`"12"`）   | 整数として比べる（`id = 12`）  |
    /// | 整数として読めない文字列       | 必ず 0 件（`1 = 0`）           |
    ///
    /// `req.model::<Post>("post")` や `req.auth().user::<User>()` は、外から来た
    /// 文字列をここへ渡します。文字列のまま比べると、PostgreSQL だけ
    /// `bigint = text` の SQL エラー（500）になるためです（[`Model::INTEGER_KEY`]）。
    /// 主キー以外の列は、渡した値のまま比べます。
    pub fn where_(self, column: &str, value: impl IntoValue) -> Self {
        match self.key_condition(column, value.into_value()) {
            Some(value) => self.map(|q| q.where_(column, value)),
            None => self.nothing(column, false),
        }
    }

    /// 列と値を演算子で比べる。
    ///
    /// 整数の主キーの列を `=` `!=` `<>` `<` `<=` `>` `>=` で比べるときは、
    /// [`where_`](Self::where_) と同じく値を直します。整数として読めない文字列は、
    /// `!=` `<>` なら必ず当たる条件（`1 = 1`）、それ以外なら必ず 0 件（`1 = 0`）です。
    pub fn where_op(self, column: &str, operator: &str, value: impl IntoValue) -> Self {
        self.compare(false, column, operator, value.into_value())
    }

    /// `or` でつないで、列が値と等しい。整数の主キーの扱いは [`where_`](Self::where_) と同じです。
    pub fn or_where(self, column: &str, value: impl IntoValue) -> Self {
        match self.key_condition(column, value.into_value()) {
            Some(value) => self.map(|q| q.or_where(column, value)),
            None => self.nothing(column, true),
        }
    }

    /// `or` でつないで、列と値を演算子で比べる。整数の主キーの扱いは
    /// [`where_op`](Self::where_op) と同じです。
    pub fn or_where_op(self, column: &str, operator: &str, value: impl IntoValue) -> Self {
        self.compare(true, column, operator, value.into_value())
    }

    /// 列が一覧のどれかと等しい。
    ///
    /// 整数の主キーの列では、一覧の値を1つずつ直します。整数として読めない文字列は
    /// 当たる行が無いので外します。全部外れたときは必ず 0 件（`1 = 0`）です。
    pub fn where_in<V: IntoValue + Clone>(self, column: &str, values: &[V]) -> Self {
        match self.key_list(column, values) {
            Some(values) => self.map(|q| q.where_in(column, &values)),
            None => self.map(|q| q.where_in(column, values)),
        }
    }

    /// 列が一覧のどれとも等しくない。
    ///
    /// 整数の主キーの列では、一覧の値を1つずつ直します。整数として読めない文字列は
    /// どの行とも等しくないので外します。全部外れたときは必ず当たる条件（`1 = 1`）です。
    pub fn where_not_in<V: IntoValue + Clone>(self, column: &str, values: &[V]) -> Self {
        match self.key_list(column, values) {
            Some(values) => self.map(|q| q.where_not_in(column, &values)),
            None => self.map(|q| q.where_not_in(column, values)),
        }
    }

    /// `or` でつないで、列が一覧のどれかと等しい。整数の主キーの扱いは
    /// [`where_in`](Self::where_in) と同じです。
    pub fn or_where_in<V: IntoValue + Clone>(self, column: &str, values: &[V]) -> Self {
        match self.key_list(column, values) {
            Some(values) => self.map(|q| q.or_where_in(column, &values)),
            None => self.map(|q| q.or_where_in(column, values)),
        }
    }

    /// 主キーの列に渡す値を、主キーの型に合わせて直す。
    /// **当たる行が無いと分かっている値のときは `None`。**
    ///
    /// 整数の主キーでない列と、`INTEGER_KEY` が偽のモデルは、そのまま返します。
    /// `NULL` もそのまま返します（`is null` への読み替えは `QueryBuilder` がします）。
    /// 直し方は `find` と同じです（`lookup_key`）。
    fn key_condition(&self, column: &str, value: Value) -> Option<Value> {
        if !(T::INTEGER_KEY && is_primary_key::<T>(column)) {
            return Some(value);
        }
        let value = T::key_value(value);
        if never_matches_integer_key(&value, self.inner.driver()) {
            return None;
        }
        Some(value)
    }

    /// 一覧と比べるときの `key_condition`。
    ///
    /// 直す必要が無い列では `None`（渡された一覧をそのまま使う）。
    /// 主キーの列では、直した値の一覧を返します。当たらない値は外します。
    fn key_list<V: IntoValue + Clone>(&self, column: &str, values: &[V]) -> Option<Vec<Value>> {
        if !(T::INTEGER_KEY && is_primary_key::<T>(column)) {
            return None;
        }
        Some(
            values
                .iter()
                .cloned()
                .filter_map(|value| self.key_condition(column, value.into_value()))
                .collect(),
        )
    }

    /// `where_op` と `or_where_op` の中身。
    fn compare(self, or: bool, column: &str, operator: &str, value: Value) -> Self {
        // 値を直すのは、整数どうしで比べられる演算子だけ。`like` などはそのまま渡す。
        let operator_trimmed = operator.trim();
        let comparable = matches!(
            operator_trimmed,
            "=" | "!=" | "<>" | "<" | "<=" | ">" | ">="
        );
        let value = if comparable {
            match self.key_condition(column, value) {
                Some(value) => value,
                // 整数として読めない文字列とは、どの行も等しくない。
                None if matches!(operator_trimmed, "!=" | "<>") => {
                    return self.everything(column, or);
                }
                None => return self.nothing(column, or),
            }
        } else {
            value
        };
        if or {
            self.map(|q| q.or_where_op(column, operator, value))
        } else {
            self.map(|q| q.where_op(column, operator, value))
        }
    }

    /// 必ず 0 件になる条件（`1 = 0`）を置く。
    ///
    /// 空の一覧との `where_in` がこの形になる。列名の検査もそのまま通る。
    fn nothing(self, column: &str, or: bool) -> Self {
        let nothing: [i64; 0] = [];
        if or {
            self.map(|q| q.or_where_in(column, &nothing))
        } else {
            self.map(|q| q.where_in(column, &nothing))
        }
    }

    /// 必ず当たる条件（`1 = 1`）を置く。
    fn everything(self, column: &str, or: bool) -> Self {
        if or {
            // 列は主キーと分かっているので、検査は要らない。
            self.map(|q| q.or_where_raw("1 = 1", &[]))
        } else {
            // 空の一覧との `where_not_in` がこの形になる。
            let nothing: [i64; 0] = [];
            self.map(|q| q.where_not_in(column, &nothing))
        }
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

    /// 条件に加えて主キーで絞り、1件取る。
    ///
    /// `QueryBuilder::find` と違い、列名は `id` 固定ではなく主キーの名前を使います。
    /// 整数に読める文字列は整数として探します（[`Model::key_value`]）。
    ///
    /// ```ignore
    /// let post = Post::query().where_("status", "published").find(id).await?;
    /// ```
    ///
    /// **`NULL` と、整数の主キーに整数として読めない値は、問い合わせずに `None` です**
    /// （[`Model::INTEGER_KEY`]）。
    pub async fn find(&self, id: impl IntoValue) -> Result<Option<T>> {
        find_by_key(self, id.into_value()).await
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

    /// ある列だけを一覧で取る。NULL がありうる列は `Option<V>` で読んでください。
    pub async fn pluck<V: FromValue>(&self, column: &str) -> Result<Vec<V>> {
        self.inner.pluck(column).await
    }

    /// 最初の行の、ある列の値。NULL がありうる列は `Option<V>` で読んでください。
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

/// 列の名前が、このモデルの主キーを指しているか。`posts.id` のような表名付きも見ます。
fn is_primary_key<T: Model>(column: &str) -> bool {
    let column = column.trim();
    if column == T::PRIMARY_KEY {
        return true;
    }
    column
        .strip_prefix(T::TABLE)
        .and_then(|rest| rest.strip_prefix('.'))
        .is_some_and(|rest| rest == T::PRIMARY_KEY)
}

impl<T: Model> std::fmt::Debug for ModelQuery<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 中身は `QueryBuilder` に任せる。SQL と値の数に加えて、組み立ての間違い
        // （`deferred_error`）も出る。以前はここで SQL だけを出していたので、
        // 列名の間違いなどが `{:?}` で見えませんでした。
        f.debug_struct("ModelQuery")
            .field("table", &T::TABLE)
            .field("query", &self.inner)
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

    /// 決まった結果だけを返す接続。件数と採番された ID とドライバを自由に決められます。
    struct Fixed(Affected, Driver);

    impl Backend for Fixed {
        fn driver(&self) -> Driver {
            self.1
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
        fixed_on(Driver::Sqlite, affected).await
    }

    /// ドライバを決めて、決まった結果を返す接続からトランザクションを始める。
    async fn fixed_on(driver: Driver, affected: Affected) -> Transaction {
        let backend: Arc<dyn Backend> = Arc::new(Fixed(affected, driver));
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

    /// `created_at` / `updated_at` を持つモデル。更新で `created_at` を書き戻さないことを試します。
    struct Stamped {
        id: i64,
        title: String,
        created_at: String,
        updated_at: String,
    }

    impl Model for Stamped {
        const TABLE: &'static str = "stamped";
        const PRIMARY_KEY: &'static str = "id";
        const COLUMNS: &'static [&'static str] = &["id", "title", "created_at", "updated_at"];

        fn from_row(row: &Row) -> Result<Self> {
            Ok(Self {
                id: row.get("id")?,
                title: row.get("title")?,
                created_at: row.get("created_at")?,
                updated_at: row.get("updated_at")?,
            })
        }

        fn attributes(&self) -> Vec<(&'static str, Value)> {
            vec![
                ("title", Value::Text(self.title.clone())),
                ("created_at", Value::Text(self.created_at.clone())),
                ("updated_at", Value::Text(self.updated_at.clone())),
            ]
        }

        fn key(&self) -> Value {
            Value::Int(self.id)
        }

        fn set_key(&mut self, value: Value) {
            self.id = i64::from_value(&value).unwrap_or(0);
        }
    }

    /// 文字列の主キーを自分で実装したモデル。整数に直さないことを試します。
    struct Coded {
        code: String,
        title: String,
    }

    impl Model for Coded {
        const TABLE: &'static str = "coded";
        const PRIMARY_KEY: &'static str = "code";
        const COLUMNS: &'static [&'static str] = &["code", "title"];
        const INTEGER_KEY: bool = false;

        fn from_row(row: &Row) -> Result<Self> {
            Ok(Self {
                code: row.get("code")?,
                title: row.get("title")?,
            })
        }

        fn attributes(&self) -> Vec<(&'static str, Value)> {
            vec![("title", Value::Text(self.title.clone()))]
        }

        fn key(&self) -> Value {
            Value::Text(self.code.clone())
        }

        fn set_key(&mut self, value: Value) {
            self.code = String::from_value(&value).unwrap_or_default();
        }
    }

    #[test]
    fn 整数の主キーには文字列の_id_を整数で渡す() {
        // パス引数は文字列で届く。PostgreSQL は bigint = text を断る。
        assert_eq!(Post::key_value("12"), Value::Int(12));
        assert_eq!(Post::key_value(12), Value::Int(12));
        assert_eq!(Post::key_value("abc"), Value::Text("abc".into()));
        // 文字列の主キーは、数字だけでも文字列のまま。
        assert_eq!(Coded::key_value("12"), Value::Text("12".into()));
    }

    #[test]
    fn 整数の主キーに整数として読めない値は探さない() {
        for driver in [Driver::Sqlite, Driver::MySql, Driver::Postgres] {
            // 整数に読めるものは、整数にして探す。
            assert_eq!(lookup_key::<Post>("12", driver), Some(Value::Int(12)));
            assert_eq!(lookup_key::<Post>(12, driver), Some(Value::Int(12)));
            // 読めない文字列は探さない。PostgreSQL では bigint = text の 500 になる。
            assert_eq!(lookup_key::<Post>("abc", driver), None, "{driver}");
            assert_eq!(lookup_key::<Post>("12abc", driver), None, "{driver}");
            assert_eq!(lookup_key::<Post>("", driver), None, "{driver}");
            assert_eq!(lookup_key::<Post>("1.5", driver), None, "{driver}");
            // NULL の主キーの行は無い。`is null` で探さない。
            assert_eq!(lookup_key::<Post>(None::<i64>, driver), None);
            assert_eq!(lookup_key::<Coded>(None::<String>, driver), None);
            // 文字列の主キー（INTEGER_KEY = false）は、そのまま探す。
            assert_eq!(
                lookup_key::<Coded>("abc", driver),
                Some(Value::Text("abc".into()))
            );
        }

        // i64 を超える数字は、MySQL でだけ通す（`bigint unsigned` にありうる）。
        assert_eq!(
            lookup_key::<Post>(u64::MAX, Driver::MySql),
            Some(Value::Text(u64::MAX.to_string()))
        );
        assert_eq!(lookup_key::<Post>(u64::MAX, Driver::Sqlite), None);
        assert_eq!(lookup_key::<Post>(u64::MAX, Driver::Postgres), None);
        // 20 桁を超える数字は、どのドライバでも探さない。
        let huge = "999999999999999999999999";
        assert_eq!(lookup_key::<Post>(huge, Driver::MySql), None);
    }

    #[tokio::test]
    async fn 整数に読めない主キーのfindは問い合わせずに無いを返す() {
        let (found, tx) = spy().await;

        assert!(Post::on(&tx).find("abc").await.expect("読める").is_none());
        assert!(Post::on(&tx)
            .where_("title", "x")
            .find(None::<i64>)
            .await
            .expect("読める")
            .is_none());
        // クエリビルダの `find` も、NULL では問い合わせない。
        assert!(tx
            .table("posts")
            .find(None::<i64>)
            .await
            .expect("読める")
            .is_none());
        assert!(joined(&found.tx).is_empty(), "{}", joined(&found.tx));
        assert!(joined(&found.pool).is_empty());

        // 整数に読める文字列は、これまでどおり問い合わせる。
        assert!(Post::on(&tx).find("12").await.expect("読める").is_none());
        let sent = joined(&found.tx);
        assert_eq!(sent, "select * from \"posts\" where \"id\" = ? limit 1");

        // 問い合わせないときも、組み立ての間違いは知らせる。
        let message = Post::on(&tx)
            .order_by("id; drop table posts")
            .find("abc")
            .await
            .err()
            .expect("断られる")
            .to_string();
        assert!(message.contains("使えません"), "{message}");
    }

    #[tokio::test]
    async fn 既定の接続のfindとfind_or_failも問い合わせずに返す() {
        // 既定の接続には流れない（流れれば、つなぐところで別のエラーになる）。
        assert!(Post::find("abc").await.expect("読める").is_none());
        assert!(Post::find(None::<i64>).await.expect("読める").is_none());
        let error = Post::find_or_fail("abc").await.err().expect("404 になる");
        assert!(matches!(error, Error::Http { status: 404, .. }), "{error}");
    }

    #[tokio::test]
    async fn 主キーの条件に読めない文字列を渡すと必ず0件になる() {
        // `req.model` と `req.auth().user` は、この形で主キーを比べる。
        let (found, tx) = spy().await;
        let row = Post::on(&tx)
            .where_(Post::PRIMARY_KEY, Post::key_value("abc"))
            .first()
            .await
            .expect("SQL エラーにならない");
        assert!(row.is_none());
        let sent = joined(&found.tx);
        assert_eq!(sent, "select * from \"posts\" where 1 = 0 limit 1");

        // 表名付きでも同じ。値は束縛しない。
        let (sql, bindings) = Post::query().where_("posts.id", "abc").to_sql();
        assert_eq!(sql, "select * from \"posts\" where 1 = 0");
        assert!(bindings.is_empty());

        // 整数に読める値は整数で束縛する。以前は文字列のままで、
        // PostgreSQL では `bigint = text` の SQL エラー（500）になっていた。
        let (sql, bindings) = Post::query().where_("id", "12").to_sql();
        assert_eq!(sql, "select * from \"posts\" where \"id\" = ?");
        assert_eq!(bindings, vec![Value::Int(12)]);
        let (_, bindings) = Post::query().where_("posts.id", " 12 ").to_sql();
        assert_eq!(bindings, vec![Value::Int(12)]);
        // 主キー以外の列は、これまでどおり渡した値のまま。
        let (sql, _) = Post::query().where_("title", "abc").to_sql();
        assert_eq!(sql, "select * from \"posts\" where \"title\" = ?");
        let (sql, _) = Post::query().where_("comments.id", "abc").to_sql();
        assert_eq!(sql, "select * from \"posts\" where \"comments\".\"id\" = ?");

        // 文字列の主キーは、そのまま比べる。
        let (sql, bindings) = Coded::query().where_("code", "a1").to_sql();
        assert_eq!(sql, "select * from \"coded\" where \"code\" = ?");
        assert_eq!(bindings, vec![Value::Text("a1".into())]);

        // 列名の検査は、読み替えたときも効く。
        assert!(is_primary_key::<Post>(" id "));
        assert!(!is_primary_key::<Post>("posts.title"));
        assert!(!is_primary_key::<Post>("postsid"));
    }

    #[test]
    fn 主キーを取るほかの条件も値を直す() {
        // or_where も where_ と同じ。
        let (sql, bindings) = Post::query()
            .where_("title", "x")
            .or_where("id", "12")
            .or_where("id", "abc")
            .to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" where \"title\" = ? or \"id\" = ? or 1 = 0"
        );
        assert_eq!(bindings, vec![Value::Text("x".into()), Value::Int(12)]);

        // 比べる演算子では直す。読めない文字列は `!=` だけ必ず当たる。
        let (sql, bindings) = Post::query()
            .where_op("id", ">", "5")
            .where_op("id", "=", "abc")
            .or_where_op("id", "<>", "abc")
            .where_op("id", " != ", "abc")
            .or_where_op("id", "<=", "abc")
            .to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" where \"id\" > ? and 1 = 0 or 1 = 1 and 1 = 1 or 1 = 0"
        );
        assert_eq!(bindings, vec![Value::Int(5)]);

        // `like` などはそのまま渡す。演算子の検査も今までどおり効く。
        let (_, bindings) = Post::query().where_op("id", "like", "1%").to_sql();
        assert_eq!(bindings, vec![Value::Text("1%".into())]);
        let text = format!("{:?}", Post::query().where_op("id", "in", "abc"));
        assert!(text.contains("値1つとは比べられません"), "{text}");
        // NULL は `is null` / `is not null` のまま。
        let (sql, _) = Post::query().where_op("id", "!=", None::<i64>).to_sql();
        assert_eq!(sql, "select * from \"posts\" where \"id\" is not null");

        // 一覧は1つずつ直し、読めない値は外す。
        let (sql, bindings) = Post::query().where_in("id", &["1", "abc", " 3 "]).to_sql();
        assert_eq!(sql, "select * from \"posts\" where \"id\" in (?, ?)");
        assert_eq!(bindings, vec![Value::Int(1), Value::Int(3)]);
        let (sql, bindings) = Post::query().where_not_in("posts.id", &["2", "x"]).to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" where \"posts\".\"id\" not in (?)"
        );
        assert_eq!(bindings, vec![Value::Int(2)]);

        // 全部外れたら、空の一覧と同じ（`in` は 1 = 0、`not in` は 1 = 1）。
        let (sql, bindings) = Post::query()
            .where_in("id", &["abc"])
            .where_not_in("id", &["x", "y"])
            .or_where_in("id", &["z"])
            .to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" where 1 = 0 and 1 = 1 or 1 = 0"
        );
        assert!(bindings.is_empty());

        // 主キー以外の列と、文字列の主キーは、渡した値のまま。
        let (_, bindings) = Post::query().where_in("title", &["1", "abc"]).to_sql();
        assert_eq!(
            bindings,
            vec![Value::Text("1".into()), Value::Text("abc".into())]
        );
        let (_, bindings) = Coded::query()
            .where_in("code", &["12", "a1"])
            .where_op("code", "=", "007")
            .to_sql();
        assert_eq!(
            bindings,
            vec![
                Value::Text("12".into()),
                Value::Text("a1".into()),
                Value::Text("007".into())
            ]
        );
    }

    #[tokio::test]
    async fn mysqlではi64を超える主キーも条件に残す() {
        // MySQL の `id()` は `bigint unsigned` なので、その値の行がありうる。
        // find（never_matches_integer_key）と同じ扱いにそろえる。
        let big = u64::MAX.to_string();
        let tx = fixed_on(Driver::MySql, affected()).await;
        let (sql, bindings) = Post::on(&tx)
            .where_in("id", &[big.as_str(), "abc"])
            .to_sql();
        assert_eq!(sql, "select * from `posts` where `id` in (?)");
        assert_eq!(bindings, vec![Value::Text(big.clone())]);
        // SQLite と PostgreSQL では当たる行が無い。
        let (sql, _) = Post::query().where_("id", big.as_str()).to_sql();
        assert_eq!(sql, "select * from \"posts\" where 1 = 0");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn 主キーの条件を文字列で渡しても実際に当たる() {
        use super::super::{ConnectionConfig, Schema};

        let backend = super::super::sqlite::connect(&ConnectionConfig::sqlite("test", ":memory:"))
            .await
            .expect("メモリ上の SQLite につながる");
        let mut schema = Schema::new(Driver::Sqlite);
        schema.create("posts", |t| {
            t.id();
            t.string("title");
        });
        for sql in schema.into_statements() {
            backend.execute(&sql, &[]).await.expect("表が作れる");
        }
        let tx = Transaction::start(backend).await.expect("始められる");
        for title in ["1本目", "2本目", "3本目"] {
            tx.table("posts")
                .insert(&[("title", Value::Text(title.into()))])
                .await
                .expect("入る");
        }

        let posts = Post::on(&tx)
            .where_in("id", &["1", "abc", "3"])
            .order_by("id")
            .get()
            .await
            .expect("取れる");
        let ids: Vec<i64> = posts.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![1, 3]);

        let count = Post::on(&tx)
            .where_("id", "2")
            .or_where_op("id", "!=", "abc")
            .count()
            .await
            .expect("数えられる");
        assert_eq!(count, 3);
        let count = Post::on(&tx)
            .where_not_in("id", &["x"])
            .count()
            .await
            .expect("数えられる");
        assert_eq!(count, 3);
    }

    #[test]
    fn モデルの条件でもnullは_is_null_になる() {
        let none: Option<String> = None;
        let (sql, bindings) = Post::query()
            .where_("deleted_at", none.clone())
            .or_where("title", none.clone())
            .where_op("body", "!=", none)
            .to_sql();
        assert_eq!(
            sql,
            "select * from \"posts\" where \"deleted_at\" is null \
             or \"title\" is null and \"body\" is not null"
        );
        assert!(bindings.is_empty());
    }

    #[tokio::test]
    async fn usingで替えたら元の接続の間違いは残らない() {
        // 設定に無い接続で組み立てても、有効なトランザクションへ替えれば動く。
        let (found, tx) = spy().await;
        let query = ModelQuery::<Post>::new(Source::Named("無い接続".to_string()))
            .where_("title", "x")
            .using(&tx);
        query.get().await.expect("トランザクションで動く");
        assert_eq!(
            joined(&found.tx),
            "select * from \"posts\" where \"title\" = ?"
        );
    }

    #[tokio::test]
    async fn 件数が0のページ分けは本体を問い合わせない() {
        // Spy は行を返さないので、件数は 0 になる。
        let (found, tx) = spy().await;
        let page = Post::on(&tx)
            .where_("title", "x")
            .latest()
            .paginate(15, 2)
            .await
            .expect("取れる");
        assert_eq!(page.total, 0);
        assert!(page.is_empty());
        assert_eq!(page.current_page, 2);
        assert_eq!(
            joined(&found.tx),
            "select count(*) as bengara_aggregate from \"posts\" where \"title\" = ?",
            "流れるのは件数の 1 本だけ"
        );
    }

    #[tokio::test]
    async fn 主キーを自分で決める行のsaveは入れ方を案内する() {
        // 主キーに値があるので update になり、0 件で断られる。
        let tx = fixed(Affected {
            rows: 0,
            last_insert_id: None,
        })
        .await;
        let mut coded = Coded {
            code: "a1".into(),
            title: "x".into(),
        };
        let message = coded
            .save_using(&tx)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("coded.code = a1"), "{message}");
        assert!(message.contains("DB::table(..).insert(..)"), "{message}");

        // 採番が返らない表（MySQL の `last_insert_id()` が 0）でも、同じ入れ方を案内する。
        // MySQL の接続は 0 を「番号なし」（`None`）にして返す。以前はここで
        // 「insert が ID を返しませんでした」とだけ出ていた。
        let tx = fixed_on(
            Driver::MySql,
            Affected {
                rows: 1,
                last_insert_id: None,
            },
        )
        .await;
        let mut post = Post {
            id: 0,
            title: "x".into(),
        };
        let message = post
            .save_using(&tx)
            .await
            .expect_err("断られる")
            .to_string();
        assert!(message.contains("行は入っています"), "{message}");
        assert!(message.contains("posts"), "{message}");
        assert!(message.contains("DB::table(..).insert(..)"), "{message}");
        // 以前の案内。その通りにすると「行が無い」で落ちるので、出さない。
        assert!(!message.contains("値を入れてから"), "{message}");
    }

    #[test]
    fn find_は主キーの名前で絞る() {
        // ModelQuery::find は `id` 固定ではなく PRIMARY_KEY を使う。
        let query = Coded::query().where_("title", "x");
        let (sql, _) = query
            .inner
            .clone()
            .where_(Coded::PRIMARY_KEY, Coded::key_value("a1"))
            .to_sql();
        assert_eq!(
            sql,
            "select * from \"coded\" where \"title\" = ? and \"code\" = ?"
        );
    }

    #[tokio::test]
    async fn 更新では_created_at_を書き戻さない() {
        let (found, tx) = spy().await;
        let mut row = Stamped {
            id: 3,
            title: "やきそば".into(),
            // 読み直さずに作った構造体。以前はこの空文字で上書きしていた。
            created_at: String::new(),
            updated_at: String::new(),
        };
        row.save_using(&tx).await.expect("更新できる");
        let sent = joined(&found.tx);
        assert!(
            sent.contains(
                "update \"stamped\" set \"title\" = ?, \"updated_at\" = ? where \"id\" = ?"
            ),
            "{sent}"
        );
        assert!(!sent.contains("\"created_at\" = ?"), "{sent}");

        // 新しい行では入れる。
        let mut fresh = Stamped {
            id: 0,
            title: "たこやき".into(),
            created_at: "2026-10-29 00:00:00".into(),
            updated_at: "2026-10-29 00:00:00".into(),
        };
        fresh.save_using(&tx).await.expect("保存できる");
        let sent = joined(&found.tx);
        assert!(
            sent.contains("insert into \"stamped\" (\"title\", \"created_at\", \"updated_at\")"),
            "{sent}"
        );
    }

    #[tokio::test]
    async fn 文字列の主キーの更新は文字列のまま束縛する() {
        let (found, tx) = spy().await;
        let mut coded = Coded {
            code: "007".into(),
            title: "x".into(),
        };
        coded.save_using(&tx).await.expect("更新できる");
        assert!(joined(&found.tx).contains("where \"code\" = ?"));
        // 束縛した値は文字列のまま（Spy は値を覚えないので、key_value で確かめる）。
        assert_eq!(Coded::key_value(coded.key()), Value::Text("007".into()));
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
        assert!(text.contains("select * from"), "{text}");
    }

    #[test]
    fn デバッグ表示に組み立ての間違いも出る() {
        // 以前は SQL だけを出していたので、列名の間違いが見えなかった。
        let text = format!("{:?}", Post::query().order_by("id; drop table posts"));
        assert!(text.contains("ModelQuery"), "{text}");
        assert!(text.contains("deferred_error"), "{text}");
        assert!(text.contains("使えません"), "{text}");
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

    /// 閲覧数と `updated_at` を持つモデル。`save_only` を試します。
    ///
    /// `touch_timestamps` は `#[derive(Model)]` と同じく `updated_at` を入れます。
    struct Counted {
        id: i64,
        title: String,
        views: i64,
        updated_at: String,
    }

    impl Model for Counted {
        const TABLE: &'static str = "counted";
        const PRIMARY_KEY: &'static str = "id";
        const COLUMNS: &'static [&'static str] = &["id", "title", "views", "updated_at"];

        fn from_row(row: &Row) -> Result<Self> {
            Ok(Self {
                id: row.get("id")?,
                title: row.get("title")?,
                views: row.get("views")?,
                updated_at: row.get("updated_at")?,
            })
        }

        fn attributes(&self) -> Vec<(&'static str, Value)> {
            vec![
                ("title", Value::Text(self.title.clone())),
                ("views", Value::Int(self.views)),
                ("updated_at", Value::Text(self.updated_at.clone())),
            ]
        }

        fn key(&self) -> Value {
            Value::Int(self.id)
        }

        fn set_key(&mut self, value: Value) {
            self.id = i64::from_value(&value).unwrap_or(0);
        }

        fn touch_timestamps(&mut self, _creating: bool) {
            self.updated_at = "touched".into();
        }
    }

    /// `counted` の表を持つメモリ上の SQLite で、トランザクションを始める。
    #[cfg(feature = "sqlite")]
    async fn counted_table() -> Transaction {
        use super::super::{ConnectionConfig, Schema};

        let backend = super::super::sqlite::connect(&ConnectionConfig::sqlite("test", ":memory:"))
            .await
            .expect("メモリ上の SQLite につながる");
        let mut schema = Schema::new(Driver::Sqlite);
        schema.create("counted", |t| {
            t.id();
            t.string("title");
            t.integer("views").default(0);
            t.string("updated_at").default("");
        });
        for sql in schema.into_statements() {
            backend.execute(&sql, &[]).await.expect("表が作れる");
        }
        Transaction::start(backend).await.expect("始められる")
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn save_onlyは指定した列とupdated_atだけを書く() {
        let tx = counted_table().await;
        tx.table("counted")
            .insert(&[("title", Value::Text("やきそば".into()))])
            .await
            .expect("入る");
        let mut post = Counted::on(&tx)
            .find(1)
            .await
            .expect("読める")
            .expect("ある");

        // 読んだ後で、他の人が閲覧数を増やした。
        tx.table("counted")
            .where_("id", 1)
            .increment("views", 5)
            .await
            .expect("増える");

        post.title = "たこやき".into();
        let affected = post.save_only_using(&tx, &["title"]).await.expect("書ける");
        assert_eq!(affected, 1);
        assert_eq!(
            post.updated_at, "touched",
            "構造体の updated_at も今の時刻になる"
        );

        let saved = Counted::on(&tx)
            .find(1)
            .await
            .expect("読める")
            .expect("ある");
        assert_eq!(saved.title, "たこやき");
        assert_eq!(saved.views, 5, "閲覧数は巻き戻らない");
        assert_eq!(saved.updated_at, "touched", "updated_at は自動で足す");

        // 比べるために save() も試す。読んだときの閲覧数（0）で書き戻す。
        post.save_using(&tx).await.expect("書ける");
        let clobbered = Counted::on(&tx)
            .find(1)
            .await
            .expect("読める")
            .expect("ある");
        assert_eq!(clobbered.views, 0, "save() は全列を書き戻す");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn save_onlyは行が無ければ0を返す() {
        let tx = counted_table().await;
        let mut gone = Counted {
            id: 42,
            title: "消えた行".into(),
            views: 0,
            updated_at: String::new(),
        };
        let affected = gone
            .save_only_using(&tx, &["title"])
            .await
            .expect("エラーにはしない");
        assert_eq!(affected, 0);
        // トランザクションは使い続けられる（失敗扱いにしていない）。
        assert_eq!(tx.table("counted").count().await.expect("数えられる"), 0);
    }

    #[tokio::test]
    async fn save_onlyは列の間違いを問い合わせる前に断る() {
        let (found, tx) = spy().await;
        let mut post = Counted {
            id: 1,
            title: "x".into(),
            views: 0,
            updated_at: "before".into(),
        };

        let message = post
            .save_only_using(&tx, &["nope"])
            .await
            .expect_err("無い列は断る")
            .to_string();
        assert!(message.contains("nope"), "{message}");

        let message = post
            .save_only_using(&tx, &["id"])
            .await
            .expect_err("主キーは断る")
            .to_string();
        assert!(message.contains("主キー"), "{message}");

        let message = post
            .save_only_using(&tx, &[])
            .await
            .expect_err("空は断る")
            .to_string();
        assert!(message.contains("1つ以上"), "{message}");

        let mut fresh = Counted {
            id: 0,
            title: "x".into(),
            views: 0,
            updated_at: String::new(),
        };
        let message = fresh
            .save_only_using(&tx, &["title"])
            .await
            .expect_err("新しい行は断る")
            .to_string();
        assert!(message.contains("save()"), "{message}");

        assert_eq!(post.updated_at, "before", "断るときは構造体を書き換えない");
        assert!(joined(&found.tx).is_empty(), "何も流さない");
    }

    #[tokio::test]
    async fn save_onlyの文は指定した列と主キーの条件だけ() {
        let (found, tx) = spy().await;
        let mut post = Counted {
            id: 3,
            title: "x".into(),
            views: 9,
            updated_at: String::new(),
        };
        post.save_only_using(&tx, &["title"]).await.expect("書ける");
        let sent = joined(&found.tx);
        assert_eq!(
            sent,
            "update \"counted\" set \"title\" = ?, \"updated_at\" = ? where \"id\" = ?"
        );
    }
}

//! bengara の手続きマクロ。
//!
//! - `#[bengara::test]`：テスト関数に付ける。
//! - `#[derive(Model)]`：構造体と表を結びつける。

mod model;

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

/// `app/Models/` の構造体に付けて、表と結びつける。
///
/// ```ignore
/// #[derive(Model)]
/// #[model(table = "posts")]
/// pub struct Post {
///     pub id: i64,
///     pub title: String,
/// }
/// ```
///
/// | 指定 | 置き場所 | 既定 |
/// |---|---|---|
/// | `#[model(table = "posts")]` | 構造体 | 必須 |
/// | `#[model(primary)]` | フィールド | 列名が `id` の項目 |
/// | `#[model(column = "名前")]` | フィールド | フィールド名と同じ |
/// | `#[model(skip)]` | フィールド | 表にない項目として扱う |
#[proc_macro_derive(Model, attributes(model))]
pub fn derive_model(item: TokenStream) -> TokenStream {
    model::derive(item)
}

/// `tests/` 以下のテスト関数に付ける。
///
/// ```ignore
/// #[bengara::test]
/// async fn home_page_is_ok() {
///     get("/").await.assert_ok().assert_see("bengara");
/// }
/// ```
///
/// 次のことをまとめて行います。
///
/// - `.env` と `config/` を読み込む（1回だけ）
/// - `bootstrap/app.rs` のアプリを組み立てる
/// - tokio のランタイムを用意して、本体を直接呼ぶ
/// - 関数の中で `client`、`get`、`post`、`put`、`patch`、`delete`、
///   `refresh_database`、`seed_database`、`exclusive` を使えるようにする
#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return syn::Error::new_spanned(
            proc_macro2::TokenStream::from(attr),
            "#[bengara::test] は引数を取りません",
        )
        .to_compile_error()
        .into();
    }

    let input = parse_macro_input!(item as ItemFn);

    if input.sig.asyncness.is_none() {
        return syn::Error::new_spanned(
            input.sig.fn_token,
            "#[bengara::test] は `async fn` に付けてください",
        )
        .to_compile_error()
        .into();
    }
    if !input.sig.inputs.is_empty() {
        return syn::Error::new_spanned(
            input.sig.inputs,
            "#[bengara::test] を付けた関数は引数を取れません",
        )
        .to_compile_error()
        .into();
    }
    // 戻り値は捨てられます。黙って捨てると `async fn x() -> Result<()>` のときに
    // 「期待される型は `()`」という、このマクロが見えないエラーになります。
    if let syn::ReturnType::Type(_, ty) = &input.sig.output {
        return syn::Error::new_spanned(
            ty,
            "#[bengara::test] を付けた関数は値を返せません。\
             戻り値の型を書かずに、関数の中で結果を確かめてください",
        )
        .to_compile_error()
        .into();
    }
    // 型引数も捨てられます。`Generics` の `params` を見るので、ライフタイムと
    // const 引数も同じ文で断ります。
    if !input.sig.generics.params.is_empty() {
        return syn::Error::new_spanned(
            &input.sig.generics,
            "#[bengara::test] を付けた関数は型引数やライフタイムを取れません",
        )
        .to_compile_error()
        .into();
    }
    if let Some(where_clause) = &input.sig.generics.where_clause {
        return syn::Error::new_spanned(
            where_clause,
            "#[bengara::test] を付けた関数に `where` は書けません",
        )
        .to_compile_error()
        .into();
    }

    let attrs = &input.attrs;
    let vis = &input.vis;
    let name = &input.sig.ident;
    let body = &input.block;

    quote! {
        #[::core::prelude::v1::test]
        #(#attrs)*
        #vis fn #name() {
            ::bengara::testing::run(
                crate::__bengara_hooks(),
                || crate::bootstrap::app::app(),
                |__bengara_client| async move {
                    // テストの中でそのまま使える道具を用意する。
                    #[allow(unused_variables)]
                    let client = __bengara_client;
                    #[allow(unused_variables)]
                    let get = |__uri: &str| {
                        let __uri = ::std::string::String::from(__uri);
                        let __client = ::core::clone::Clone::clone(&client);
                        async move { __client.get(&__uri).await }
                    };
                    // DB を使うテストは、これを `let _db = refresh_database().await;` で受け取る。
                    #[allow(unused_variables)]
                    let refresh_database = || async { ::bengara::testing::refresh_database().await };
                    // シーダーは札（`refresh_database()` の戻り）を受け取る。
                    // 札を取っていないテストから呼べないように、型で縛ってある。
                    #[allow(unused_imports)]
                    use ::bengara::testing::{exclusive, seed_database};
                    #[allow(unused_variables)]
                    let post = |__uri: &str, __body: &str| {
                        let __uri = ::std::string::String::from(__uri);
                        let __body = ::std::string::String::from(__body);
                        let __client = ::core::clone::Clone::clone(&client);
                        async move { __client.post(&__uri, &__body).await }
                    };
                    // PUT / PATCH / DELETE。Route::put などに合わせて揃えてある。
                    // 本文の形は post と同じ。中身は TestClient 側で決めている。
                    #[allow(unused_variables)]
                    let put = |__uri: &str, __body: &str| {
                        let __uri = ::std::string::String::from(__uri);
                        let __body = ::std::string::String::from(__body);
                        let __client = ::core::clone::Clone::clone(&client);
                        async move { __client.put(&__uri, &__body).await }
                    };
                    #[allow(unused_variables)]
                    let patch = |__uri: &str, __body: &str| {
                        let __uri = ::std::string::String::from(__uri);
                        let __body = ::std::string::String::from(__body);
                        let __client = ::core::clone::Clone::clone(&client);
                        async move { __client.patch(&__uri, &__body).await }
                    };
                    #[allow(unused_variables)]
                    let delete = |__uri: &str| {
                        let __uri = ::std::string::String::from(__uri);
                        let __client = ::core::clone::Clone::clone(&client);
                        async move { __client.delete(&__uri).await }
                    };
                    #body
                },
            )
        }
    }
    .into()
}

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
/// | `#[model(primary)]` | フィールド | `id` という名前のフィールド |
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
/// - 関数の中で `client`、`get`、`post`、`refresh_database`、`seed_database` を使えるようにする
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
                    #[allow(unused_variables)]
                    let seed_database = || async { ::bengara::testing::seed_database().await };
                    #[allow(unused_variables)]
                    let post = |__uri: &str, __body: &str| {
                        let __uri = ::std::string::String::from(__uri);
                        let __body = ::std::string::String::from(__body);
                        let __client = ::core::clone::Clone::clone(&client);
                        async move { __client.post(&__uri, &__body).await }
                    };
                    #body
                },
            )
        }
    }
    .into()
}

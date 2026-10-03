//! bengara の手続きマクロ。
//!
//! 今あるのは `#[bengara::test]` だけです。

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

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
/// - 関数の中で `client`、`get`、`post` を使えるようにする
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

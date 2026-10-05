//! `#[derive(Model)]` の実装。
//!
//! 生成するのは `impl bengara::database::Model for ...` の1つだけです。
//! 読み出し（`from_row`）と書き込み（`attributes`）の対応を作ります。

use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{parse_macro_input, Data, DeriveInput, Fields, LitStr};

/// 1つのフィールドの情報。
struct Field {
    ident: syn::Ident,
    /// フィールドの型。主キーが整数かを見るために持っています。
    ty: syn::Type,
    /// 列の名前。
    column: String,
    /// 主キーか。
    primary: bool,
    /// 表には無い項目か。
    skip: bool,
}

/// 自動採番の主キーに使える型。
///
/// いまの作りは「insert の後に、データベースが決めた番号を書き戻す」ものなので、
/// 整数だけに対応します。
const INTEGER_TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32", "u64", "usize",
];

pub(crate) fn derive(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    match expand(input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn expand(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "#[derive(Model)] は型引数のある構造体には使えません",
        ));
    }

    let table = table_name(&input)?;
    let fields = collect_fields(&input)?;

    let used: Vec<&Field> = fields.iter().filter(|f| !f.skip).collect();
    if used.is_empty() {
        return Err(syn::Error::new(
            input.span(),
            "#[derive(Model)] には、表の列になるフィールドが1つ以上必要です",
        ));
    }

    check_duplicate_columns(&used)?;

    let primary = pick_primary(&used, &input)?;
    check_primary_type(primary)?;
    let primary_column = primary.column.clone();
    let primary_ident = primary.ident.clone();

    // save() は主キー以外の列を insert / update します。1つも無いと実行できません。
    // 実行のときに初めて分かるのでは遅いので、ここで止めます。
    if !used.iter().any(|f| f.column != primary_column) {
        return Err(syn::Error::new(
            input.ident.span(),
            format!(
                "{name} には主キー `{primary_column}` 以外の列がありません。\
                 save() で入れるものが無いので、列になるフィールドを1つ以上置いてください。\
                 表には無い項目には `#[model(skip)]` を付けます"
            ),
        ));
    }

    let columns: Vec<LitStr> = used
        .iter()
        .map(|f| LitStr::new(&f.column, f.ident.span()))
        .collect();

    // from_row: 列を読んでフィールドに入れる。
    let reads = fields.iter().map(|field| {
        let ident = &field.ident;
        if field.skip {
            return quote! { #ident: ::core::default::Default::default() };
        }
        let column = LitStr::new(&field.column, ident.span());
        quote! { #ident: __row.get(#column)? }
    });

    // attributes: 主キー以外の列と値。
    // 主キーは常に外します。自動採番なので、insert で送るとデータベースが決めた番号と
    // 食い違います（整数の主キーだけに対応しているのは、そのためです）。
    // `#[model(primary)]` を書かずに `id` を使っている場合もあるので、列名で比べる。
    let primary_name = primary_column.clone();
    let writes = used
        .iter()
        .filter(|f| f.column != primary_name)
        .map(|field| {
            let ident = &field.ident;
            let column = LitStr::new(&field.column, ident.span());
            quote! {
                (
                    #column,
                    ::bengara::database::IntoValue::into_value(
                        ::core::clone::Clone::clone(&self.#ident),
                    ),
                )
            }
        });

    // created_at / updated_at があれば、保存のときに今の時刻を入れる。
    let has_created = used.iter().any(|f| f.column == "created_at");
    let has_updated = used.iter().any(|f| f.column == "updated_at");
    let touch = if has_created || has_updated {
        let created = if has_created {
            let ident = used
                .iter()
                .find(|f| f.column == "created_at")
                .map(|f| f.ident.clone())
                .expect("直前に確かめている");
            quote! {
                if __creating {
                    self.#ident = ::core::convert::Into::into(
                        ::core::clone::Clone::clone(&__now),
                    );
                }
            }
        } else {
            quote! {}
        };
        let updated = if has_updated {
            let ident = used
                .iter()
                .find(|f| f.column == "updated_at")
                .map(|f| f.ident.clone())
                .expect("直前に確かめている");
            quote! { self.#ident = ::core::convert::Into::into(__now); }
        } else {
            quote! {}
        };
        quote! {
            fn touch_timestamps(&mut self, __creating: bool) {
                let __now = ::bengara::database::now();
                #created
                #updated
            }
        }
    } else {
        quote! {}
    };

    let table_lit = LitStr::new(&table, input.ident.span());
    let primary_lit = LitStr::new(&primary_column, primary_ident.span());

    Ok(quote! {
        impl ::bengara::database::Model for #name {
            const TABLE: &'static str = #table_lit;
            const PRIMARY_KEY: &'static str = #primary_lit;
            const COLUMNS: &'static [&'static str] = &[#(#columns),*];

            fn from_row(__row: &::bengara::database::Row) -> ::bengara::Result<Self> {
                ::core::result::Result::Ok(Self { #(#reads),* })
            }

            fn attributes(&self) -> ::std::vec::Vec<(&'static str, ::bengara::database::Value)> {
                ::std::vec![#(#writes),*]
            }

            fn key(&self) -> ::bengara::database::Value {
                ::bengara::database::IntoValue::into_value(
                    ::core::clone::Clone::clone(&self.#primary_ident),
                )
            }

            fn set_key(&mut self, __value: ::bengara::database::Value) {
                match ::bengara::database::FromValue::from_value(&__value) {
                    ::core::result::Result::Ok(__v) => self.#primary_ident = __v,
                    // 読めなかったときは値を変えない。
                    // 黙って既定値（0 など）で上書きすると、別の行を指してしまう。
                    ::core::result::Result::Err(__e) => {
                        ::bengara::database::warn_key_not_set(
                            #table_lit,
                            #primary_lit,
                            &__e,
                        );
                    }
                }
            }

            #touch
        }
    })
}

/// `#[model(table = "...")]` を読む。
fn table_name(input: &DeriveInput) -> syn::Result<String> {
    let mut table = None;
    for attr in &input.attrs {
        if !attr.path().is_ident("model") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                let value: LitStr = meta.value()?.parse()?;
                table = Some(value.value());
                return Ok(());
            }
            Err(meta.error("構造体に付けられるのは `table = \"表の名前\"` だけです"))
        })?;
    }
    table.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            "表の名前が決まっていません。`#[model(table = \"posts\")]` を付けてください",
        )
    })
}

/// フィールドの指定を読む。
fn collect_fields(input: &DeriveInput) -> syn::Result<Vec<Field>> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            input.ident.span(),
            "#[derive(Model)] は名前付きのフィールドを持つ構造体にだけ使えます",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new(
            data.fields.span(),
            "#[derive(Model)] は名前付きのフィールドを持つ構造体にだけ使えます",
        ));
    };

    let mut out = Vec::new();
    for field in &named.named {
        let ident = field
            .ident
            .clone()
            .ok_or_else(|| syn::Error::new(field.span(), "フィールドに名前がありません"))?;
        let mut info = Field {
            column: ident.to_string(),
            ident,
            ty: field.ty.clone(),
            primary: false,
            skip: false,
        };
        for attr in &field.attrs {
            if !attr.path().is_ident("model") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("primary") {
                    info.primary = true;
                    return Ok(());
                }
                if meta.path.is_ident("skip") {
                    info.skip = true;
                    return Ok(());
                }
                if meta.path.is_ident("column") {
                    let value: LitStr = meta.value()?.parse()?;
                    info.column = value.value();
                    return Ok(());
                }
                Err(meta.error(
                    "フィールドに付けられるのは `primary` / `skip` / `column = \"名前\"` だけです",
                ))
            })?;
        }
        // `skip` は「表には無い項目」、`primary` は「表の主キー」。両方は成り立たない。
        // 以前は skip が先に効いて、primary が黙って無視されていた。
        if info.skip && info.primary {
            return Err(syn::Error::new(
                info.ident.span(),
                "`#[model(skip)]` と `#[model(primary)]` は同じフィールドには付けられません。\
                 表に無い項目なら `skip` だけ、主キーなら `primary` だけにしてください",
            ));
        }
        out.push(info);
    }
    Ok(out)
}

/// 同じ列名が2回出てこないか。
///
/// `COLUMNS` と `attributes()` に同じ名前が2つ入ると、`insert` が落ちます。
fn check_duplicate_columns(fields: &[&Field]) -> syn::Result<()> {
    for (index, field) in fields.iter().enumerate() {
        if let Some(found) = fields[..index].iter().find(|f| f.column == field.column) {
            return Err(syn::Error::new(
                field.ident.span(),
                format!(
                    "列 `{}` が2回出てきます（`{}` と `{}`）。\
                     `#[model(column = \"別の名前\")]` で分けるか、\
                     表に無い項目なら `#[model(skip)]` を付けてください",
                    field.column, found.ident, field.ident
                ),
            ));
        }
    }
    Ok(())
}

/// 主キーの型が整数か。
fn check_primary_type(primary: &Field) -> syn::Result<()> {
    if is_integer_type(&primary.ty) {
        return Ok(());
    }
    Err(syn::Error::new(
        primary.ty.span(),
        format!(
            "主キー `{}` の型が整数ではありません。\
             いまは自動採番の整数の主キーだけに対応しています。\n\
             使えるのは {} です。\n\
             insert の後にデータベースが決めた番号を書き戻す作りなので、\
             自分で決めた主キー（文字列・UUID など）は送れません。\n\
             どうしても使いたいときは、`#[derive(Model)]` をやめて \
             `bengara::database::Model` を自分で実装してください。",
            primary.column,
            INTEGER_TYPES.join(" / ")
        ),
    ))
}

/// 整数の型か。`i64` のような、型引数の無い名前だけを見ます。
fn is_integer_type(ty: &syn::Type) -> bool {
    let syn::Type::Path(path) = ty else {
        return false;
    };
    if path.qself.is_some() {
        return false;
    }
    let Some(last) = path.path.segments.last() else {
        return false;
    };
    if !last.arguments.is_empty() {
        return false;
    }
    INTEGER_TYPES.contains(&last.ident.to_string().as_str())
}

/// 主キーを決める。`#[model(primary)]` が無ければ `id` を使う。
fn pick_primary<'a>(fields: &[&'a Field], input: &DeriveInput) -> syn::Result<&'a Field> {
    let marked: Vec<&'a Field> = fields.iter().copied().filter(|f| f.primary).collect();
    if marked.len() > 1 {
        return Err(syn::Error::new(
            marked[1].ident.span(),
            "#[model(primary)] は1つのフィールドにだけ付けてください",
        ));
    }
    if let Some(found) = marked.first() {
        return Ok(found);
    }
    fields
        .iter()
        .find(|f| f.column == "id")
        .copied()
        .ok_or_else(|| {
            syn::Error::new(
                input.ident.span(),
                "主キーが決まっていません。`id` という名前のフィールドを置くか、\
                 `#[model(primary)]` を付けてください",
            )
        })
}

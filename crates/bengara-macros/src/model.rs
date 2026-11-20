//! `#[derive(Model)]` の実装。
//!
//! 生成するのは `impl bengara::database::Model for ...` の1つだけです。
//! 読み出し（`from_row`）と書き込み（`attributes`）の対応を作ります。

use proc_macro::TokenStream;
use quote::quote;
use syn::ext::IdentExt;
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
            // 見ているのは `generics.params` なので、ライフタイムと const 引数も断ります。
            "#[derive(Model)] は型引数やライフタイムのある構造体には使えません",
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
    //
    // 「あるか」と「どれか」を別々に調べると、列名の決め方を変えたときに
    // 片方だけ直してマクロがパニックします。最初から `find` の結果で分けます。
    let created = used
        .iter()
        .find(|f| f.column == "created_at")
        .map(|f| f.ident.clone());
    let updated = used
        .iter()
        .find(|f| f.column == "updated_at")
        .map(|f| f.ident.clone());
    let touch = if created.is_some() || updated.is_some() {
        let created = match &created {
            Some(ident) => quote! {
                if __creating {
                    self.#ident = ::core::convert::Into::into(
                        ::core::clone::Clone::clone(&__now),
                    );
                }
            },
            None => quote! {},
        };
        let updated = match &updated {
            Some(ident) => quote! { self.#ident = ::core::convert::Into::into(__now); },
            None => quote! {},
        };
        quote! {
            fn touch_timestamps(&mut self, __creating: bool) {
                let __now = ::bengara::Carbon::now();
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
                check_name(&value, NameKind::Table)?;
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
            // `r#type` のような生の識別子は、`r#` を外した名前を列名にする。
            // そのまま文字にすると列名が `r#type` になり、読み出しは「列がありません」、
            // 保存は「列名は使えません」で落ちていた。
            column: ident.unraw().to_string(),
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
                    check_name(&value, NameKind::Column)?;
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

/// 検査する名前の種類。
#[derive(Clone, Copy)]
enum NameKind {
    /// `#[model(table = "..")]`。`スキーマ.表` の形（`.` 区切り）も通します。
    Table,
    /// `#[model(column = "..")]`。`.` は通しません。
    Column,
}

/// `table` / `column` に書いた名前を、コンパイルのときに確かめる。
///
/// 規則は実行時の検査（`bengara` の `database/grammar.rs`）と同じです。
/// 実行時の検査は最初の問い合わせまで走らないので、書き間違いをビルドで止めます。
///
/// | 名前 | 実行時の関数 | 通すもの |
/// |---|---|---|
/// | 表 | `is_plain_identifier` | 英数字（Unicode）と `_`。`.` 区切り（各部は空にできない） |
/// | 列 | `is_bare_identifier` | 英数字（Unicode）と `_`。`.` は通さない |
///
/// 実行時と違うのは1点だけです。**前後の空白も断ります。** 実行時は前後の空白を
/// 見ずに通しますが、そのまま引用符でくくるので `" posts"` という別の名前になり、
/// 必ず「表がありません」で終わります。
///
/// 別のクレートなので関数は共有できません（マクロのクレートは本体に依存できない）。
/// 規則を変えるときは両方を直してください。
fn check_name(literal: &LitStr, kind: NameKind) -> syn::Result<()> {
    match name_problem(&literal.value(), kind) {
        Some(problem) => Err(syn::Error::new(literal.span(), problem)),
        None => Ok(()),
    }
}

/// 名前に問題があれば、その説明を返す。
fn name_problem(name: &str, kind: NameKind) -> Option<String> {
    let (what, example) = match kind {
        NameKind::Table => ("表の名前", "#[model(table = \"posts\")]"),
        NameKind::Column => ("列の名前", "#[model(column = \"title\")]"),
    };
    if name.is_empty() {
        return Some(format!(
            "{what}が空です。`{example}` のように書いてください"
        ));
    }
    let allow_dot = matches!(kind, NameKind::Table);
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_alphanumeric() || *c == '_' || (allow_dot && *c == '.')))
    {
        let hint = match (kind, bad) {
            (NameKind::Column, '.') => {
                "列の名前に表の名前は付けられません。列の名前だけを書いてください"
            }
            (NameKind::Table, _) => "使えるのは英数字と `_`、スキーマを分ける `.` だけです",
            (NameKind::Column, _) => "使えるのは英数字と `_` だけです",
        };
        return Some(format!(
            "{what} `{name}` に使えない文字 `{}` が入っています。{hint}",
            bad.escape_debug()
        ));
    }
    if name.split('.').any(str::is_empty) {
        return Some(format!(
            "{what} `{name}` の `.` の前後が空です。`スキーマ.表` の形で書いてください"
        ));
    }
    None
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
                "主キーが決まっていません。列名が `id` の項目を置くか、\
                 `#[model(primary)]` を付けてください",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 構造体の定義を展開する。成功なら生成コード（文字）、失敗ならエラーの文。
    ///
    /// 手続きマクロの入口（`derive`）はコンパイラの中でしか動かないので、
    /// その1つ内側の `expand` を試します。
    fn run(source: &str) -> Result<String, String> {
        let input: DeriveInput =
            syn::parse_str(source).expect("テストの入力が構文として読めません");
        expand(input)
            .map(|tokens| tokens.to_string())
            .map_err(|error| error.to_string())
    }

    /// 生成コードから、空白の違いを消す（`quote` の出す空白に頼らないため）。
    fn squeeze(code: &str) -> String {
        code.chars().filter(|c| !c.is_whitespace()).collect()
    }

    #[test]
    fn 表と列の対応を作る() {
        let code = squeeze(
            &run(
                "#[model(table = \"posts\")] struct Post { id: i64, title: String, \
                  #[model(column = \"body_text\")] body: String, #[model(skip)] cache: u8 }",
            )
            .unwrap(),
        );
        assert!(code.contains("constTABLE:&'staticstr=\"posts\";"), "{code}");
        assert!(
            code.contains("constPRIMARY_KEY:&'staticstr=\"id\";"),
            "{code}"
        );
        // `skip` は列に入らない。`column` は列名だけを変える。
        assert!(code.contains("&[\"id\",\"title\",\"body_text\"]"), "{code}");
        assert!(code.contains("body:__row.get(\"body_text\")?"), "{code}");
        assert!(
            code.contains("cache:::core::default::Default::default()"),
            "{code}"
        );
    }

    #[test]
    fn 生の識別子は印を外した名前を列名にする() {
        // 以前は列名が `r#type` になり、読み出しも保存も実行時に落ちていた。
        let code = squeeze(
            &run("#[model(table = \"posts\")] struct Post { id: i64, r#type: String }").unwrap(),
        );
        assert!(code.contains("&[\"id\",\"type\"]"), "{code}");
        // フィールドの側は `r#type` のまま使う（予約語なので外せない）。
        assert!(code.contains("r#type:__row.get(\"type\")?"), "{code}");
        assert!(code.contains("(\"type\","), "{code}");
        assert!(code.contains("&self.r#type"), "{code}");
        assert!(!code.contains("\"r#type\""), "{code}");
    }

    #[test]
    fn 生の識別子の主キーも列名で見つける() {
        // `r#id` と書いても、列名 `id` として主キーになる。
        let code = squeeze(
            &run("#[model(table = \"posts\")] struct Post { r#id: i64, title: String }").unwrap(),
        );
        assert!(
            code.contains("constPRIMARY_KEY:&'staticstr=\"id\";"),
            "{code}"
        );
    }

    #[test]
    fn 使える名前はそのまま通す() {
        for table in ["posts", "main.posts", "記事", "posts_2"] {
            assert_eq!(name_problem(table, NameKind::Table), None, "{table}");
        }
        for column in ["title", "題名", "views_2", "_hidden"] {
            assert_eq!(name_problem(column, NameKind::Column), None, "{column}");
        }
    }

    #[test]
    fn 使えない名前はビルドで止める() {
        // 実行時の `is_plain_identifier`（表）と `is_bare_identifier`（列）が断る形。
        for table in [
            "",
            "my posts",
            "posts;",
            "\"posts\"",
            "po-sts",
            "count(*)",
            ".posts",
            "posts.",
            "a..b",
            " posts",
            "posts\n",
        ] {
            assert!(name_problem(table, NameKind::Table).is_some(), "{table:?}");
        }
        for column in [
            "",
            "posts.title",
            "ti tle",
            "title`",
            "r#type",
            "*",
            "title ",
        ] {
            assert!(
                name_problem(column, NameKind::Column).is_some(),
                "{column:?}"
            );
        }
    }

    #[test]
    fn 名前のエラーは何が悪いかを言う() {
        let error =
            run("#[model(table = \"\")] struct Post { id: i64, title: String }").unwrap_err();
        assert!(error.contains("表の名前が空です"), "{error}");

        let error = run("#[model(table = \"my posts\")] struct Post { id: i64, title: String }")
            .unwrap_err();
        assert!(error.contains("`my posts`"), "{error}");
        assert!(error.contains("使えない文字 ` `"), "{error}");

        let error = run("#[model(table = \"posts\")] struct Post { id: i64, \
                         #[model(column = \"posts.title\")] title: String }")
        .unwrap_err();
        assert!(error.contains("列の名前 `posts.title`"), "{error}");
        assert!(error.contains("表の名前は付けられません"), "{error}");

        let error = run("#[model(table = \"posts\")] struct Post { id: i64, \
                         #[model(column = \"\")] title: String }")
        .unwrap_err();
        assert!(error.contains("列の名前が空です"), "{error}");

        let error =
            run("#[model(table = \"a..b\")] struct Post { id: i64, title: String }").unwrap_err();
        assert!(error.contains("前後が空"), "{error}");
    }

    #[test]
    fn 今までの検査はそのまま効く() {
        let error = run("struct Post { id: i64, title: String }").unwrap_err();
        assert!(error.contains("表の名前が決まっていません"), "{error}");

        let error = run("#[model(table = \"posts\")] struct Post { id: String, title: String }")
            .unwrap_err();
        assert!(error.contains("整数ではありません"), "{error}");

        let error = run(
            "#[model(table = \"posts\")] struct Post { id: i64, title: String, \
                         #[model(column = \"title\")] other: String }",
        )
        .unwrap_err();
        assert!(error.contains("2回出てきます"), "{error}");

        let error =
            run("#[model(table = \"posts\")] struct Post<T> { id: i64, title: T }").unwrap_err();
        assert!(error.contains("型引数"), "{error}");
    }
}

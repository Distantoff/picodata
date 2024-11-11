use quote::quote;

macro_rules! unwrap_or_compile_error {
    ($expr:expr) => {
        match $expr {
            Ok(v) => v,
            Err(e) => {
                return e.to_compile_error().into();
            }
        }
    };
}

#[proc_macro]
pub fn format_but_ignore_everything_after_semicolon(
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let args: proc_macro::TokenStream = input
        .into_iter()
        .take_while(
            |token| !matches!(token, proc_macro::TokenTree::Punct(punct) if punct.as_char() == ';'),
        )
        .collect();

    let args = proc_macro2::TokenStream::from(args);
    quote! {
        ::std::format!(#args)
    }
    .into()
}

#[proc_macro]
pub fn get_doc_literal(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as syn::Meta);
    let syn::Meta::NameValue(meta) = input else {
        return syn::Error::new_spanned(input, "expected a doc comment")
            .to_compile_error()
            .into();
    };
    let lit = meta.lit;
    quote! { #lit }.into()
}

#[allow(clippy::single_match)]
#[proc_macro_derive(Introspection, attributes(introspection))]
pub fn derive_introspection(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    let name = &input.ident;

    let args = unwrap_or_compile_error!(Args::from_attributes(input.attrs));

    let mut context = Context {
        args,
        fields: vec![],
    };
    match input.data {
        syn::Data::Struct(ds) => match ds.fields {
            syn::Fields::Named(fs) => {
                for mut field in fs.named {
                    let attrs = std::mem::take(&mut field.attrs);
                    let attrs = unwrap_or_compile_error!(FieldAttrs::from_attributes(attrs));
                    if attrs.ignore {
                        continue;
                    }

                    context.fields.push(FieldInfo {
                        name: field_name(&field),
                        ident: field
                            .ident
                            .clone()
                            .expect("Fields::Named has fields with names"),
                        attrs,
                        field,
                    });
                }
            }
            _ => {}
        },
        _ => {}
    }
    let crate_ = &context.args.crate_;

    let body_for_field_infos = generate_body_for_field_infos(&context);

    let body_for_set_field_from_yaml = generate_body_for_set_field_from_something(
        &context,
        &syn::parse2(quote! { set_field_from_yaml }).unwrap(),
        &syn::parse2(quote! { serde_yaml::from_str }).unwrap(),
    );

    let body_for_set_field_from_rmpv = generate_body_for_set_field_from_something(
        &context,
        &syn::parse2(quote! { set_field_from_rmpv }).unwrap(),
        &syn::parse2(quote! { #crate_::introspection::from_rmpv_value }).unwrap(),
    );

    let body_for_get_field_as_rmpv = generate_body_for_get_field_as_rmpv(&context);

    let body_for_get_field_default_value_as_rmpv =
        generate_body_for_get_field_default_value_as_rmpv(&context);

    let body_for_get_sbroad_type_of_field = generate_body_for_get_sbroad_type_of_field(&context);

    let crate_ = &context.args.crate_;
    quote! {
        #[automatically_derived]
        impl #crate_::introspection::Introspection for #name {
            const FIELD_INFOS: &'static [#crate_::introspection::FieldInfo] = &[
                #body_for_field_infos
            ];

            fn set_field_from_yaml(&mut self, path: &str, value: &str) -> ::std::result::Result<(), #crate_::introspection::IntrospectionError> {
                use #crate_::introspection::IntrospectionError;
                #body_for_set_field_from_yaml
            }

            fn set_field_from_rmpv(&mut self, path: &str, value: &#crate_::introspection::RmpvValue) -> ::std::result::Result<(), #crate_::introspection::IntrospectionError> {
                use #crate_::introspection::IntrospectionError;
                #body_for_set_field_from_rmpv
            }

            fn get_field_as_rmpv(&self, path: &str) -> ::std::result::Result<#crate_::introspection::RmpvValue, #crate_::introspection::IntrospectionError> {
                use #crate_::introspection::IntrospectionError;
                #body_for_get_field_as_rmpv
            }

            fn get_field_default_value_as_rmpv(&self, path: &str) -> Result<Option<#crate_::introspection::RmpvValue>, #crate_::introspection::IntrospectionError> {
                use #crate_::introspection::IntrospectionError;
                #body_for_get_field_default_value_as_rmpv
            }

            fn get_sbroad_type_of_field(path: &str) -> Result<Option<#crate_::config::SbroadType>, #crate_::introspection::IntrospectionError> {
                use #crate_::introspection::IntrospectionError;
                #body_for_get_sbroad_type_of_field
            }
        }
    }
    .into()
}

/// Generates body of `Introspection::FIELD_INFOS` constants array.
fn generate_body_for_field_infos(context: &Context) -> proc_macro2::TokenStream {
    let crate_ = &context.args.crate_;

    let mut code = quote! {};

    for field in &context.fields {
        let name = &field.name;
        #[allow(non_snake_case)]
        let Type = &field.field.ty;

        if !field.attrs.nested {
            code.extend(quote! {
                #crate_::introspection::FieldInfo {
                    name: #name,
                    nested_fields: &[],
                },
            });
        } else {
            code.extend(quote! {
                #crate_::introspection::FieldInfo {
                    name: #name,
                    nested_fields: #Type::FIELD_INFOS,
                },
            });
        }
    }

    code
}

/// Generates body of `Introspection::set_field_from_yaml` or `Introspection::set_field_from_rmpv` method.
/// Or may be used for other methods also if we add those.
fn generate_body_for_set_field_from_something(
    context: &Context,
    fn_ident: &syn::Ident,
    conversion_fn: &syn::Path,
) -> proc_macro2::TokenStream {
    let mut set_non_nestable = quote! {};
    let mut set_nestable = quote! {};
    let mut error_if_nestable = quote! {};
    let mut non_nestable_names = vec![];
    for field in &context.fields {
        let name = &field.name;
        let ident = &field.ident;
        #[allow(non_snake_case)]
        let Type = &field.field.ty;

        if !field.attrs.nested {
            non_nestable_names.push(name);

            // Handle assigning to a non-nestable field
            set_non_nestable.extend(quote! {
                #name => {
                    match #conversion_fn(value) {
                        Ok(v) => {
                            self.#ident = v;
                            return Ok(());
                        }
                        Err(error) => {
                            return Err(IntrospectionError::ConvertToFieldError { field: path.into(), error: error.into() });
                        }
                    }
                }
            });
        } else {
            // Handle assigning to a nested field
            set_nestable.extend(quote! {
                #name => {
                    return self.#ident.#fn_ident(tail, value)
                        .map_err(|e| e.with_prepended_prefix(head));
                }
            });

            // Handle if trying to assign to field marked with `#[introspection(nested)]`
            // This is not currently supported, all of it's subfields must be assigned individually
            error_if_nestable.extend(quote! {
                #name => return Err(IntrospectionError::AssignToNested {
                    field: path.into(),
                    example: if let Some(field) = #Type::FIELD_INFOS.get(0) {
                        field.name
                    } else {
                        "<actually there's no fields in this struct :(>"
                    },
                }),
            })
        }
    }

    // Handle if a nested path is specified for non-nestable field
    let mut error_if_non_nestable = quote! {};
    if !non_nestable_names.is_empty() {
        error_if_non_nestable = quote! {
            #( #non_nestable_names )|* => {
                return Err(IntrospectionError::NotNestable { field: head.into() })
            }
        };
    }

    // Actual generated body:
    quote! {
        match path.split_once('.') {
            Some((head, tail)) => {
                let head = head.trim();
                if head.is_empty() {
                    return Err(IntrospectionError::InvalidPath {
                        expected: "expected a field name before",
                        path: format!(".{tail}"),
                    })
                }
                let tail = tail.trim();
                if !tail.chars().next().map_or(false, char::is_alphabetic) {
                    return Err(IntrospectionError::InvalidPath {
                        expected: "expected a field name after",
                        path: format!("{head}."),
                    })
                }
                match head {
                    #error_if_non_nestable
                    #set_nestable
                    _ => {
                        return Err(IntrospectionError::NoSuchField {
                            parent: "".into(),
                            field: head.into(),
                            expected: Self::FIELD_INFOS,
                        });
                    }
                }
            }
            None => {
                match path {
                    #set_non_nestable
                    #error_if_nestable
                    _ => {
                        return Err(IntrospectionError::NoSuchField {
                            parent: "".into(),
                            field: path.into(),
                            expected: Self::FIELD_INFOS,
                        });
                    }
                }
            }
        }
    }
}

/// Generates body of `Introspection::get_field_as_rmpv` method.
fn generate_body_for_get_field_as_rmpv(context: &Context) -> proc_macro2::TokenStream {
    let crate_ = &context.args.crate_;

    let mut get_non_nestable = quote! {};
    let mut get_whole_nestable = quote! {};
    let mut get_nested_subfield = quote! {};
    let mut non_nestable_names = vec![];
    for field in &context.fields {
        let name = &field.name;
        let ident = &field.ident;
        #[allow(non_snake_case)]
        let Type = &field.field.ty;

        if !field.attrs.nested {
            non_nestable_names.push(name);

            // Handle getting a non-nestable field
            get_non_nestable.extend(quote! {
                #name => {
                    match #crate_::introspection::to_rmpv_value(&self.#ident) {
                        Err(e) => {
                            return Err(IntrospectionError::ToRmpvValue { field: path.into(), details: e });
                        }
                        Ok(value) => return Ok(value),
                    }
                }
            });
        } else {
            // Handle getting a field marked with `#[introspection(nested)]`.
            get_whole_nestable.extend(quote! {
                #name => {
                    use #crate_::introspection::RmpvValue;
                    let field_infos = #Type::FIELD_INFOS;
                    let mut fields = Vec::with_capacity(field_infos.len());
                    for sub_field in field_infos {
                        let key = RmpvValue::from(sub_field.name);
                        let value = self.#ident.get_field_as_rmpv(sub_field.name)
                            .map_err(|e| e.with_prepended_prefix(#name))?;
                        fields.push((key, value));
                    }
                    return Ok(RmpvValue::Map(fields));
                }
            });

            // Handle getting a nested field
            get_nested_subfield.extend(quote! {
                #name => {
                    return self.#ident.get_field_as_rmpv(tail)
                        .map_err(|e| e.with_prepended_prefix(head));
                }
            });
        }
    }

    // Handle if a nested path is specified for non-nestable field
    let mut error_if_non_nestable = quote! {};
    if !non_nestable_names.is_empty() {
        error_if_non_nestable = quote! {
            #( #non_nestable_names )|* => {
                return Err(IntrospectionError::NotNestable { field: head.into() })
            }
        };
    }

    // Actual generated body:
    quote! {
        match path.split_once('.') {
            Some((head, tail)) => {
                let head = head.trim();
                if head.is_empty() {
                    return Err(IntrospectionError::InvalidPath {
                        expected: "expected a field name before",
                        path: format!(".{tail}"),
                    })
                }
                let tail = tail.trim();
                if !tail.chars().next().map_or(false, char::is_alphabetic) {
                    return Err(IntrospectionError::InvalidPath {
                        expected: "expected a field name after",
                        path: format!("{head}."),
                    })
                }
                match head {
                    #error_if_non_nestable
                    #get_nested_subfield
                    _ => {
                        return Err(IntrospectionError::NoSuchField {
                            parent: "".into(),
                            field: head.into(),
                            expected: Self::FIELD_INFOS,
                        });
                    }
                }
            }
            None => {
                match path {
                    #get_non_nestable
                    #get_whole_nestable
                    _ => {
                        return Err(IntrospectionError::NoSuchField {
                            parent: "".into(),
                            field: path.into(),
                            expected: Self::FIELD_INFOS,
                        });
                    }
                }
            }
        }
    }
}

/// Generates body of `Introspection::get_field_default_value_as_rmpv` method.
fn generate_body_for_get_field_default_value_as_rmpv(
    context: &Context,
) -> proc_macro2::TokenStream {
    let crate_ = &context.args.crate_;

    let mut default_for_non_nestable = quote! {};
    let mut default_for_whole_nestable = quote! {};
    let mut default_for_nested_subfield = quote! {};
    let mut non_nestable_names = vec![];
    for field in &context.fields {
        let name = &field.name;
        let ident = &field.ident;
        #[allow(non_snake_case)]
        let Type = &field.field.ty;

        if !field.attrs.nested {
            non_nestable_names.push(name);

            // Handle getting default for a non-nestable field
            if let Some(default) = &field.attrs.config_default {
                default_for_non_nestable.extend(quote! {
                    #name => {
                        match #crate_::introspection::to_rmpv_value(&(#default)) {
                            Err(e) => {
                                return Err(IntrospectionError::ToRmpvValue { field: path.into(), details: e });
                            }
                            Ok(value) => return Ok(Some(value)),
                        }
                    }
                });
            } else {
                default_for_non_nestable.extend(quote! {
                    #name => { return Ok(None); }
                });
            }
        } else {
            // Handle getting a field marked with `#[introspection(nested)]`.
            default_for_whole_nestable.extend(quote! {
                #name => {
                    use #crate_::introspection::RmpvValue;
                    let field_infos = #Type::FIELD_INFOS;
                    let mut fields = Vec::with_capacity(field_infos.len());

                    for field in field_infos {
                        let value = self.#ident.get_field_default_value_as_rmpv(field.name)
                            .map_err(|e| e.with_prepended_prefix(#name))?;
                        let Some(value) = value else {
                            continue;
                        };

                        fields.push((RmpvValue::from(field.name), value));
                    }
                    if fields.is_empty() {
                        return Ok(None);
                    }

                    return Ok(Some(RmpvValue::Map(fields)));
                }
            });

            // Handle getting a nested field
            default_for_nested_subfield.extend(quote! {
                #name => {
                    return self.#ident.get_field_default_value_as_rmpv(tail)
                        .map_err(|e| e.with_prepended_prefix(head));
                }
            });
        }
    }

    // Handle if a nested path is specified for non-nestable field
    let mut error_if_non_nestable = quote! {};
    if !non_nestable_names.is_empty() {
        error_if_non_nestable = quote! {
            #( #non_nestable_names )|* => {
                return Err(IntrospectionError::NotNestable { field: head.into() })
            }
        };
    }

    // Actual generated body:
    quote! {
        match path.split_once('.') {
            Some((head, tail)) => {
                let head = head.trim();
                if head.is_empty() {
                    return Err(IntrospectionError::InvalidPath {
                        expected: "expected a field name before",
                        path: format!(".{tail}"),
                    })
                }
                let tail = tail.trim();
                if !tail.chars().next().map_or(false, char::is_alphabetic) {
                    return Err(IntrospectionError::InvalidPath {
                        expected: "expected a field name after",
                        path: format!("{head}."),
                    })
                }
                match head {
                    #error_if_non_nestable
                    #default_for_nested_subfield
                    _ => {
                        return Err(IntrospectionError::NoSuchField {
                            parent: "".into(),
                            field: head.into(),
                            expected: Self::FIELD_INFOS,
                        });
                    }
                }
            }
            None => {
                match path {
                    #default_for_non_nestable
                    #default_for_whole_nestable
                    _ => {
                        return Err(IntrospectionError::NoSuchField {
                            parent: "".into(),
                            field: path.into(),
                            expected: Self::FIELD_INFOS,
                        });
                    }
                }
            }
        }
    }
}

/// Generates body of `Introspection::get_sbroad_type_of_field` method.
fn generate_body_for_get_sbroad_type_of_field(context: &Context) -> proc_macro2::TokenStream {
    let mut sbroad_type_for_non_nestable = quote! {};
    let mut sbroad_type_for_whole_nestable = quote! {};
    let mut sbroad_type_for_nested_subfield = quote! {};
    let mut non_nestable_names = vec![];
    for field in &context.fields {
        let name = &field.name;
        #[allow(non_snake_case)]
        let Type = &field.field.ty;

        if !field.attrs.nested {
            non_nestable_names.push(name);

            // Handle getting sbroad type for a non-nestable field
            if let Some(sbroad_type) = &field.attrs.sbroad_type {
                sbroad_type_for_non_nestable.extend(quote! {
                    #name => { return Ok(Some(#sbroad_type)); }
                });
            } else {
                sbroad_type_for_non_nestable.extend(quote! {
                    #name => { return Ok(None); }
                });
            }
        } else {
            // Handle getting sbroad type for a field marked with `#[introspection(nested)]`.
            // Note that this code is exactly the same as in the non-nestable case.
            if let Some(sbroad_type) = &field.attrs.sbroad_type {
                sbroad_type_for_whole_nestable.extend(quote! {
                    #name => { return Ok(Some(#sbroad_type)); }
                });
            } else {
                sbroad_type_for_whole_nestable.extend(quote! {
                    #name => { return Ok(None); }
                });
            }

            // Handle getting sbroad type for a nested field
            sbroad_type_for_nested_subfield.extend(quote! {
                #name => {
                    return #Type::get_sbroad_type_of_field(tail)
                        .map_err(|e| e.with_prepended_prefix(head));
                }
            });
        }
    }

    // Handle if a nested path is specified for non-nestable field
    let mut error_if_non_nestable = quote! {};
    if !non_nestable_names.is_empty() {
        error_if_non_nestable = quote! {
            #( #non_nestable_names )|* => {
                return Err(IntrospectionError::NotNestable { field: head.into() })
            }
        };
    }

    // Actual generated body:
    quote! {
        match path.split_once('.') {
            Some((head, tail)) => {
                let head = head.trim();
                if head.is_empty() {
                    return Err(IntrospectionError::InvalidPath {
                        expected: "expected a field name before",
                        path: format!(".{tail}"),
                    })
                }
                let tail = tail.trim();
                if !tail.chars().next().map_or(false, char::is_alphabetic) {
                    return Err(IntrospectionError::InvalidPath {
                        expected: "expected a field name after",
                        path: format!("{head}."),
                    })
                }
                match head {
                    #error_if_non_nestable
                    #sbroad_type_for_nested_subfield
                    _ => {
                        return Err(IntrospectionError::NoSuchField {
                            parent: "".into(),
                            field: head.into(),
                            expected: Self::FIELD_INFOS,
                        });
                    }
                }
            }
            None => {
                match path {
                    #sbroad_type_for_non_nestable
                    #sbroad_type_for_whole_nestable
                    _ => {
                        return Err(IntrospectionError::NoSuchField {
                            parent: "".into(),
                            field: path.into(),
                            expected: Self::FIELD_INFOS,
                        });
                    }
                }
            }
        }
    }
}

struct Context {
    fields: Vec<FieldInfo>,
    args: Args,
}

struct FieldInfo {
    name: String,
    ident: syn::Ident,
    attrs: FieldAttrs,
    #[allow(unused)]
    field: syn::Field,
}

struct Args {
    crate_: syn::Path,
}

impl Args {
    fn from_attributes(attrs: Vec<syn::Attribute>) -> Result<Self, syn::Error> {
        let mut result = Self {
            crate_: syn::parse2(quote!(crate)).unwrap(),
        };

        for attr in attrs {
            if !attr.path.is_ident("introspection") {
                continue;
            }

            let meta: PathKeyValue = attr.parse_args()?;
            if meta.key.is_ident("crate") {
                result.crate_ = meta.value;
            }
        }

        Ok(result)
    }
}

#[derive(Default)]
struct FieldAttrs {
    /// Looks like this in the source code: `#[introspection(ignore)]`.
    ///
    /// If specified, the field is invisible to the derive macro, i.e. it is not
    /// present in `FIELD_INFOS` and is not settable/gettable by the
    /// setter/getter methods.
    ignore: bool,

    /// Looks like this in the source code: `#[introspection(nested)]`.
    ///
    /// If specified, the field is treated as a nested substruct for the
    /// purposes of setting/getting nested fields. The type of this field also
    /// will be required to implement the `Introspection` trait.
    ///
    /// Basically for the paths like `"foo.bar.baz"` to work with the setters and getters
    /// both `foo` field and `foo.bar` field must be marked with `#[introspection(nested)]` attribute.
    nested: bool,

    /// Looks like this in the source code: `#[introspection(config_default = <expr>)]`.
    ///
    /// If specified, the provided expression is associated with the given field
    /// as a default configuration parameter value. See also doc comments of
    /// `Introspection::get_field_default_value_as_rmpv` for more details.
    config_default: Option<syn::Expr>,

    /// Looks like this in the source code: `#[introspection(sbroad_type = <expr>)]`.
    ///
    /// The provided expression must have type `picodata::config::SbroadType`.
    /// The user must make sure that `config_default` is not conflicting with `sbroad_type`.
    sbroad_type: Option<syn::Expr>,
}

impl FieldAttrs {
    fn from_attributes(attrs: Vec<syn::Attribute>) -> Result<Self, syn::Error> {
        let mut result = Self::default();

        for attr in &attrs {
            if !attr.path.is_ident("introspection") {
                continue;
            }

            attr.parse_args_with(|input: syn::parse::ParseStream| {
                // `input` is a stream of those tokens right there
                // `#[introspection(foo, bar, ...)]`
                //                  ^^^^^^^^^^^^^
                while !input.is_empty() {
                    let ident = input.parse::<syn::Ident>()?;
                    if ident == "ignore" {
                        result.ignore = true;
                    } else if ident == "nested" {
                        result.nested = true;
                    } else if ident == "config_default" {
                        if result.config_default.is_some() {
                            return Err(syn::Error::new(ident.span(), "duplicate `config_default` specified"));
                        }

                        input.parse::<syn::Token![=]>()?;

                        result.config_default = Some(input.parse::<syn::Expr>()?);
                    } else if ident == "sbroad_type" {
                        if result.sbroad_type.is_some() {
                            return Err(syn::Error::new(ident.span(), "duplicate `sbroad_type` specified"));
                        }

                        input.parse::<syn::Token![=]>()?;

                        result.sbroad_type = Some(input.parse::<syn::Expr>()?);
                    } else {
                        return Err(syn::Error::new(
                            ident.span(),
                            format!("unknown attribute argument `{ident}`, expected one of `ignore`, `nested`, `config_default`, `sbroad_type`"),
                        ));
                    }

                    if !input.is_empty() {
                        input.parse::<syn::Token![,]>()?;
                    }
                }

                Ok(())
            })?;
        }

        Ok(result)
    }
}

fn field_name(field: &syn::Field) -> String {
    // TODO: consider using `quote::format_ident!` instead
    let mut name = field.ident.as_ref().unwrap().to_string();
    if name.starts_with("r#") {
        // Remove 2 leading characters
        name.remove(0);
        name.remove(0);
    }
    name
}

#[derive(Debug)]
struct PathKeyValue {
    key: syn::Path,
    #[allow(unused)]
    eq_token: syn::Token![=],
    value: syn::Path,
}

impl syn::parse::Parse for PathKeyValue {
    fn parse(input: syn::parse::ParseStream) -> Result<Self, syn::Error> {
        Ok(Self {
            key: input.parse()?,
            eq_token: input.parse()?,
            value: input.parse()?,
        })
    }
}

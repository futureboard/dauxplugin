//! `#[derive(DauxState)]` — save and restore a struct through the DAUx state container.
//!
//! The generated code is deliberately boring: one `put_*` call per annotated field in
//! declaration order, and one checked read per field on the way back. Two properties are
//! worth stating because they are what makes a saved project survive a plug-in update:
//!
//! * **Keys, not offsets.** A field is stored under its key, so reordering fields, or
//!   adding one with `default`, does not invalidate an existing blob.
//! * **Checked conversions.** Integers travel as `i64` and come back through `TryFrom`,
//!   so a hostile or truncated blob produces a `StateError` rather than a silently
//!   wrapped number.

use proc_macro2::{Span, TokenStream};
use quote::{ToTokens, quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{
    Data, DeriveInput, Error, Expr, Fields, GenericArgument, Ident, LitStr, Path, PathArguments,
    Result, Type, Visibility,
};

use crate::attr::AttrSet;

/// How the field attribute is spelled, for diagnostics.
const FIELD_CONTEXT: &str = "#[state(..)]";
/// How the container attribute is spelled, for diagnostics.
const CONTAINER_CONTEXT: &str = "#[state(..)] on the struct";

/// Every key `#[state(..)]` accepts on a field.
const FIELD_KEYS: &[&str] = &["key", "kind", "default", "nested", "skip"];

/// Every key `#[state(..)]` accepts on the container.
const CONTAINER_KEYS: &[&str] = &["version", "group", "crate"];

/// How one field is stored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StateKind {
    /// A `f64` entry.
    Float,
    /// An `i64` entry, converted with `TryFrom` in both directions.
    Int,
    /// A `bool` entry.
    Bool,
    /// A UTF-8 string entry.
    Str,
    /// An opaque byte-string entry.
    Bytes,
    /// A group holding another `#[derive(DauxState)]` struct.
    Nested,
}

impl StateKind {
    /// The `kind = ".."` spelling of each storage kind.
    const NAMES: &'static [(&'static str, Self)] = &[
        ("f64", Self::Float),
        ("i64", Self::Int),
        ("bool", Self::Bool),
        ("str", Self::Str),
        ("bytes", Self::Bytes),
    ];

    /// Recognises the storage kind from the field's *written* type.
    ///
    /// Only the last path segment is inspected, so `std::string::String` and `String`
    /// are both understood. A type alias is not: a macro sees names, never types.
    pub(crate) fn from_type(ty: &Type) -> Option<Self> {
        let Type::Path(path) = ty else {
            return None;
        };
        if path.qself.is_some() {
            return None;
        }
        let segment = path.path.segments.last()?;
        match segment.ident.to_string().as_str() {
            "f32" | "f64" => Some(Self::Float),
            "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64"
            | "u128" | "usize" => Some(Self::Int),
            "bool" => Some(Self::Bool),
            "String" => Some(Self::Str),
            "Vec" => is_vec_of_u8(&segment.arguments).then_some(Self::Bytes),
            _ => None,
        }
    }
}

/// `true` when the generic arguments are exactly `<u8>`.
fn is_vec_of_u8(arguments: &PathArguments) -> bool {
    let PathArguments::AngleBracketed(args) = arguments else {
        return false;
    };
    let mut types = args.args.iter().filter_map(|arg| match arg {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    });
    let Some(Type::Path(path)) = types.next() else {
        return false;
    };
    types.next().is_none() && path.qself.is_none() && path.path.is_ident("u8")
}

/// One saved field.
pub(crate) struct StateField {
    /// The field's name, also the identifier used in generated code.
    ident: Ident,
    /// The field's written type, used for casts and diagnostics.
    ty: Type,
    /// The storage key.
    key: LitStr,
    /// How it is stored.
    kind: StateKind,
    /// What to restore when the key is missing; `None` makes a missing key an error.
    default: Option<Expr>,
}

/// The whole derive input, after parsing and validation.
pub(crate) struct StateInput {
    ident: Ident,
    vis: Visibility,
    generics: syn::Generics,
    krate: Path,
    version: Option<syn::LitInt>,
    group: Option<LitStr>,
    fields: Vec<StateField>,
}

/// Entry point: parse, validate, then generate.
pub(crate) fn derive(input: &DeriveInput) -> Result<TokenStream> {
    Ok(expand(&parse(input)?))
}

// ------------------------------------------------------------------------ parsing ---

/// Parses and fully validates a `#[derive(DauxState)]` input.
pub(crate) fn parse(input: &DeriveInput) -> Result<StateInput> {
    let container = AttrSet::parse(
        &input.attrs,
        "state",
        CONTAINER_CONTEXT,
        CONTAINER_KEYS,
        input.ident.span(),
    )?;

    let Data::Struct(data) = &input.data else {
        return Err(Error::new_spanned(
            &input.ident,
            "`#[derive(DauxState)]` saves a struct with named fields\n  write \
             `struct MyState { #[state] gain: f64 }`",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(Error::new_spanned(
            &input.ident,
            "`#[derive(DauxState)]` needs named fields: a field's name is its default \
             storage key, and a key is what makes a saved project survive a reordering",
        ));
    };

    let version = match container.get("version") {
        Some(entry) => {
            let lit = entry.int_value(CONTAINER_CONTEXT)?;
            if lit.base10_parse::<u32>()? == 0 {
                return Err(Error::new(
                    lit.span(),
                    "a state version starts at 1\n  bump it when the meaning of a saved \
                     field changes, and add a migration for the old spelling",
                ));
            }
            Some(lit)
        }
        None => None,
    };

    let group = match container.get("group") {
        Some(entry) => {
            let lit = entry.str_value(CONTAINER_CONTEXT)?;
            check_key(&lit)?;
            Some(lit)
        }
        None => None,
    };

    let mut fields = Vec::new();
    for field in &named.named {
        let ident = field
            .ident
            .clone()
            .unwrap_or_else(|| unreachable!("named fields always have an identifier"));
        // `#[state]` on its own is the common case: save this field under its own name.
        let attrs = AttrSet::parse_allowing_bare(
            &field.attrs,
            "state",
            FIELD_CONTEXT,
            FIELD_KEYS,
            ident.span(),
        )?;
        if !attrs.present {
            continue;
        }
        if let Some(entry) = attrs.get("skip") {
            entry.expect_flag(FIELD_CONTEXT)?;
            if let Some(other) = attrs.first_of(&["key", "kind", "default", "nested"]) {
                return Err(Error::new(
                    other.span(),
                    format!(
                        "`{}` cannot be combined with `skip`: a skipped field is not saved\n  \
                         remove `skip`, or remove the other keys",
                        other.name()
                    ),
                ));
            }
            continue;
        }
        fields.push(parse_field(&ident, &field.ty, &attrs)?);
    }

    check_unique_keys(&input.ident, &fields)?;

    Ok(StateInput {
        ident: input.ident.clone(),
        vis: input.vis.clone(),
        generics: input.generics.clone(),
        krate: container.crate_path()?,
        version,
        group,
        fields,
    })
}

/// Parses one annotated field.
fn parse_field(ident: &Ident, ty: &Type, attrs: &AttrSet) -> Result<StateField> {
    let key = match attrs.get("key") {
        Some(entry) => {
            let lit = entry.str_value(FIELD_CONTEXT)?;
            check_key(&lit)?;
            lit
        }
        None => LitStr::new(&ident.to_string(), ident.span()),
    };

    let nested = attrs.get("nested");
    if let Some(entry) = nested {
        entry.expect_flag(FIELD_CONTEXT)?;
        attrs.reject(
            "kind",
            "a nested field is a group, and the child derive decides how its own fields are \
             stored",
        )?;
        attrs.reject(
            "default",
            "a nested field is restored by the child's `load_state_at`; give the child's \
             fields their own defaults instead",
        )?;
    }

    let kind = if nested.is_some() {
        StateKind::Nested
    } else {
        match attrs.get("kind") {
            Some(entry) => {
                let lit = entry.str_value(FIELD_CONTEXT)?;
                let written = lit.value();
                let Some((_, kind)) = StateKind::NAMES
                    .iter()
                    .find(|(name, _)| *name == written.trim())
                else {
                    let valid: Vec<&str> = StateKind::NAMES.iter().map(|(name, _)| *name).collect();
                    return Err(Error::new(
                        lit.span(),
                        format!(
                            "unknown storage kind `{written}`\n  valid kinds: {}",
                            valid.join(", ")
                        ),
                    ));
                };
                *kind
            }
            None => {
                let Some(kind) = StateKind::from_type(ty) else {
                    return Err(Error::new(
                        ty.span(),
                        format!(
                            "`#[derive(DauxState)]` cannot store `{}` on its own\n  it knows \
                             f32/f64, the integer types, bool, String and Vec<u8>\n  write \
                             `kind = \"bytes\"` to pick a codec, `nested` if `{ident}` is \
                             itself `#[derive(DauxState)]`, or `skip` if it is scratch",
                            ty.to_token_stream(),
                        ),
                    ));
                };
                kind
            }
        }
    };

    let default = match attrs.get("default") {
        Some(entry) if entry.is_flag() => {
            Some(syn::parse_quote_spanned!(entry.span() => ::core::default::Default::default()))
        }
        Some(entry) => Some(entry.expr(FIELD_CONTEXT)?.clone()),
        None => None,
    };

    Ok(StateField {
        ident: ident.clone(),
        ty: ty.clone(),
        key,
        kind,
        default,
    })
}

/// Rejects a storage key the container would reject at run time.
fn check_key(key: &LitStr) -> Result<()> {
    let text = key.value();
    if text.is_empty() {
        return Err(Error::new(key.span(), "a state key may not be empty"));
    }
    if text.contains('/') {
        return Err(Error::new(
            key.span(),
            "a state key may not contain `/`, which separates path segments\n  use `nested` \
             to write a group instead",
        ));
    }
    Ok(())
}

/// Rejects two fields that would answer to the same key, since the second would silently
/// overwrite the first.
fn check_unique_keys(struct_ident: &Ident, fields: &[StateField]) -> Result<()> {
    for (index, field) in fields.iter().enumerate() {
        let Some(earlier) = fields[..index]
            .iter()
            .find(|earlier| earlier.key.value() == field.key.value())
        else {
            continue;
        };
        let mut error = Error::new(
            field.key.span(),
            format!(
                "duplicate state key \"{}\" on `{struct_ident}`: field `{}` already uses it\n  \
                 the second write would silently replace the first, and the load would put \
                 one value in both fields",
                field.key.value(),
                earlier.ident,
            ),
        );
        error.combine(Error::new(
            earlier.key.span(),
            format!("`{}` first uses this key here", earlier.ident),
        ));
        return Err(error);
    }
    Ok(())
}

// --------------------------------------------------------------------- generation ---

/// Generates the inherent `save_state`, `load_state` and `load_state_at`.
fn expand(input: &StateInput) -> TokenStream {
    let StateInput {
        ident,
        vis,
        generics,
        krate,
        version,
        group,
        fields,
    } = input;
    let private = quote!(#krate::__private);
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let saves = fields.iter().map(|field| save_field(&private, field));
    let loads = fields.iter().map(|field| load_field(&private, field));

    let (open_group, close_group) = match group {
        Some(group) => (
            quote!(writer.begin_group(#group);),
            quote!(writer.end_group();),
        ),
        None => (TokenStream::new(), TokenStream::new()),
    };
    let root_prefix = LitStr::new(
        &group
            .as_ref()
            .map_or_else(String::new, |group| format!("{}/", group.value())),
        group.as_ref().map_or_else(Span::call_site, LitStr::span),
    );

    let schema = version.as_ref().map(|version| {
        let doc = format!(
            "`[any-thread]` The schema version `{ident}` writes, as declared by \
             `#[state(version = ..)]`.\n\nPass it to `StateWriter::new` so that the version \
             in the blob and the version the fields were written for can never drift apart."
        );
        quote! {
            #[doc = #doc]
            #vis const STATE_VERSION: #private::StateVersion = #private::StateVersion(#version);
        }
    });

    let save_doc = format!(
        "`[main-thread]` Writes every `#[state(..)]` field of `{ident}` into `writer`.\
         \n\nGenerated by `#[derive(DauxState)]`. Fields are written in declaration order \
         under their keys, so adding or reordering a field does not invalidate an existing \
         blob. Allocates; never call it from the audio thread.\n\n# Errors\n\nThe first \
         error the writer latched, or a `StateError` for an integer too large to be stored \
         as an `i64`."
    );
    let load_doc = format!(
        "`[main-thread]` Restores every `#[state(..)]` field of `{ident}` from `reader`.\
         \n\nGenerated by `#[derive(DauxState)]`. Fields keep their current value only \
         where `#[state(default)]` says so; anything else missing, of the wrong type or out \
         of range is an error rather than a silent zero. Allocates; never call it from the \
         audio thread.\n\n# Errors\n\n`StateError` when a key is missing, holds another \
         type, or holds a number that does not fit the field."
    );
    let load_at_doc = format!(
        "`[main-thread]` Restores `{ident}` from the sub-tree at `prefix`.\
         \n\n`prefix` is either empty or ends with `/`; `load_state` calls this with the \
         group named by `#[state(group = ..)]`, and a `#[state(nested)]` field calls it with \
         its own key appended. Generated by `#[derive(DauxState)]`.\n\n# Errors\n\nAs \
         `load_state`."
    );

    quote! {
        #[automatically_derived]
        impl #impl_generics #ident #ty_generics #where_clause {
            #schema

            #[doc = #save_doc]
            // Integers all travel through `TryInto` so that one code path is correct for
            // every width. Clippy points out that widening a `u16` cannot fail, which is
            // true and not worth branching the macro over — and the lint would otherwise
            // fire on the author's own field, far from anything they wrote.
            #[allow(clippy::unnecessary_fallible_conversions)]
            #vis fn save_state(&self, writer: &mut #private::StateWriter)
                -> #private::StateResult<()>
            {
                #open_group
                #(#saves)*
                #close_group
                match writer.error() {
                    ::core::option::Option::Some(error) => {
                        ::core::result::Result::Err(::core::clone::Clone::clone(error))
                    }
                    ::core::option::Option::None => ::core::result::Result::Ok(()),
                }
            }

            #[doc = #load_doc]
            #vis fn load_state(&mut self, reader: &#private::StateReader)
                -> #private::StateResult<()>
            {
                self.load_state_at(reader, #root_prefix)
            }

            #[doc = #load_at_doc]
            // `reader` and `prefix` are unused when nothing is annotated; the conversion
            // lint is answered in `save_state` above.
            #[allow(unused_variables, clippy::unnecessary_fallible_conversions)]
            #vis fn load_state_at(&mut self, reader: &#private::StateReader, prefix: &str)
                -> #private::StateResult<()>
            {
                #(#loads)*
                ::core::result::Result::Ok(())
            }
        }
    }
}

/// The statements that write one field.
fn save_field(private: &TokenStream, field: &StateField) -> TokenStream {
    let StateField { ident, ty, key, .. } = field;
    let span = ident.span();
    match field.kind {
        StateKind::Float => {
            if is_named(ty, "f64") {
                quote_spanned!(span => writer.put_f64(#key, self.#ident);)
            } else {
                quote_spanned!(span => writer.put_f64(#key, self.#ident as f64);)
            }
        }
        StateKind::Int => {
            let message = format!("field `{ident}` holds a value too large to be saved as an i64",);
            quote_spanned! { span =>
                match ::core::convert::TryInto::<i64>::try_into(self.#ident) {
                    ::core::result::Result::Ok(value) => writer.put_i64(#key, value),
                    ::core::result::Result::Err(_) => {
                        return ::core::result::Result::Err(
                            #private::StateError::corrupt(#message).with_key(#key),
                        );
                    }
                }
            }
        }
        StateKind::Bool => quote_spanned!(span => writer.put_bool(#key, self.#ident);),
        StateKind::Str => quote_spanned!(span => writer.put_str(#key, &self.#ident);),
        StateKind::Bytes => quote_spanned!(span => writer.put_bytes(#key, &self.#ident);),
        StateKind::Nested => quote_spanned! { span =>
            writer.begin_group(#key);
            self.#ident.save_state(writer)?;
            writer.end_group();
        },
    }
}

/// The statements that read one field back.
fn load_field(private: &TokenStream, field: &StateField) -> TokenStream {
    let StateField {
        ident,
        ty,
        key,
        default,
        ..
    } = field;
    let span = ident.span();

    if field.kind == StateKind::Nested {
        return quote_spanned! { span =>
            self.#ident.load_state_at(reader, &::std::format!("{}{}/", prefix, #key))?;
        };
    }

    let body = match field.kind {
        StateKind::Float => {
            let cast = if is_named(ty, "f64") {
                TokenStream::new()
            } else {
                quote_spanned!(span => as #ty)
            };
            match default {
                Some(default) => quote_spanned! { span =>
                    self.#ident = match reader.opt_f64(&path) {
                        ::core::option::Option::Some(value) => value #cast,
                        ::core::option::Option::None => #default,
                    };
                },
                None => quote_spanned!(span => self.#ident = reader.f64(&path)? #cast;),
            }
        }
        StateKind::Int => {
            let message = format!("field `{ident}` cannot hold the saved value");
            let convert = quote_spanned! { span =>
                match ::core::convert::TryInto::try_into(raw) {
                    ::core::result::Result::Ok(value) => value,
                    ::core::result::Result::Err(_) => {
                        return ::core::result::Result::Err(
                            #private::StateError::corrupt(#message).with_key(&path),
                        );
                    }
                }
            };
            match default {
                Some(default) => quote_spanned! { span =>
                    self.#ident = match reader.opt_i64(&path) {
                        ::core::option::Option::Some(raw) => #convert,
                        ::core::option::Option::None => #default,
                    };
                },
                None => quote_spanned! { span =>
                    let raw = reader.i64(&path)?;
                    self.#ident = #convert;
                },
            }
        }
        StateKind::Bool => match default {
            Some(default) => quote_spanned! { span =>
                self.#ident = match reader.opt_bool(&path) {
                    ::core::option::Option::Some(value) => value,
                    ::core::option::Option::None => #default,
                };
            },
            None => quote_spanned!(span => self.#ident = reader.bool(&path)?;),
        },
        StateKind::Str => match default {
            Some(default) => quote_spanned! { span =>
                self.#ident = match reader.opt_str(&path) {
                    ::core::option::Option::Some(value) => {
                        ::std::borrow::ToOwned::to_owned(value)
                    }
                    ::core::option::Option::None => #default,
                };
            },
            None => quote_spanned! { span =>
                self.#ident = ::std::borrow::ToOwned::to_owned(reader.str(&path)?);
            },
        },
        StateKind::Bytes => match default {
            Some(default) => quote_spanned! { span =>
                self.#ident = match reader.opt_bytes(&path) {
                    ::core::option::Option::Some(value) => ::std::vec::Vec::from(value),
                    ::core::option::Option::None => #default,
                };
            },
            None => quote_spanned! { span =>
                self.#ident = ::std::vec::Vec::from(reader.bytes(&path)?);
            },
        },
        StateKind::Nested => unreachable!("nested fields returned above"),
    };

    quote_spanned! { span =>
        {
            let path = ::std::format!("{}{}", prefix, #key);
            #body
        }
    }
}

/// `true` when the type's last path segment is exactly `name`.
fn is_named(ty: &Type, name: &str) -> bool {
    let Type::Path(path) = ty else {
        return false;
    };
    path.qself.is_none()
        && path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(source: &str) -> DeriveInput {
        syn::parse_str(source).expect("test input parses")
    }

    fn expand_str(source: &str) -> String {
        let tokens = derive(&input(source)).expect("expansion succeeds");
        syn::parse2::<syn::File>(tokens.clone())
            .unwrap_or_else(|e| panic!("generated code is not valid Rust: {e}\n{tokens}"));
        tokens.to_string()
    }

    fn error(source: &str) -> String {
        derive(&input(source))
            .expect_err("expected a compile error")
            .into_iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(" | ")
    }

    const EVERY_TYPE: &str = r#"
        pub struct Saved {
            #[state]
            gain: f64,
            #[state]
            mix: f32,
            #[state]
            voices: u32,
            #[state]
            bypass: bool,
            #[state]
            preset: String,
            #[state]
            curve: Vec<u8>,
            #[state(skip)]
            scratch: Vec<f32>,
            unannotated: u64,
        }
    "#;

    #[test]
    fn every_recognised_type_round_trips_through_its_own_codec() {
        let expanded = expand_str(EVERY_TYPE);
        assert!(
            expanded.contains("writer . put_f64 (\"gain\" , self . gain)"),
            "{expanded}"
        );
        assert!(
            expanded.contains("writer . put_f64 (\"mix\" , self . mix as f64)"),
            "{expanded}"
        );
        assert!(
            expanded.contains("put_bool (\"bypass\" , self . bypass)"),
            "{expanded}"
        );
        assert!(
            expanded.contains("put_str (\"preset\" , & self . preset)"),
            "{expanded}"
        );
        assert!(
            expanded.contains("put_bytes (\"curve\" , & self . curve)"),
            "{expanded}"
        );
        assert!(expanded.contains("reader . f64 (& path)"), "{expanded}");
        assert!(expanded.contains("reader . bool (& path)"), "{expanded}");
        assert!(expanded.contains("reader . str (& path)"), "{expanded}");
        assert!(expanded.contains("reader . bytes (& path)"), "{expanded}");
    }

    #[test]
    fn skipped_and_unannotated_fields_are_left_alone() {
        let expanded = expand_str(EVERY_TYPE);
        assert!(!expanded.contains("scratch"), "{expanded}");
        assert!(!expanded.contains("unannotated"), "{expanded}");
    }

    #[test]
    fn integers_are_converted_with_try_from_in_both_directions() {
        let expanded = expand_str(EVERY_TYPE);
        // A `u32` field that met a saved 5_000_000_000 must fail loudly, not wrap.
        assert!(
            expanded.contains("TryInto :: < i64 > :: try_into (self . voices)"),
            "{expanded}"
        );
        assert!(expanded.contains("TryInto :: try_into (raw)"), "{expanded}");
        assert!(!expanded.contains("self . voices as i64"), "{expanded}");
        assert!(!expanded.contains("as u32"), "{expanded}");
    }

    #[test]
    fn a_key_defaults_to_the_field_name_and_can_be_overridden() {
        let expanded = expand_str(
            r#"
            struct Saved {
                #[state]
                gain: f64,
                #[state(key = "wet_dry")]
                mix: f64,
            }
            "#,
        );
        assert!(expanded.contains("put_f64 (\"gain\""), "{expanded}");
        assert!(expanded.contains("put_f64 (\"wet_dry\""), "{expanded}");
        assert!(!expanded.contains("\"mix\""), "{expanded}");
    }

    #[test]
    fn a_default_makes_a_missing_key_survivable() {
        let expanded = expand_str(
            r#"
            struct Saved {
                #[state(default)]
                gain: f64,
                #[state(default = 0.5)]
                mix: f64,
                #[state(default = String::from("Init"))]
                preset: String,
            }
            "#,
        );
        assert!(expanded.contains("reader . opt_f64 (& path)"), "{expanded}");
        assert!(
            expanded.contains(":: core :: default :: Default :: default ()"),
            "{expanded}"
        );
        assert!(expanded.contains("None => 0.5"), "{expanded}");
        assert!(
            expanded.contains("None => String :: from (\"Init\")"),
            "{expanded}"
        );
        // Without a default the read must stay strict.
        let strict = expand_str("struct S { #[state] gain: f64 }");
        assert!(!strict.contains("opt_f64"), "{strict}");
    }

    #[test]
    fn a_nested_field_becomes_a_group_on_both_sides() {
        let expanded = expand_str(
            r#"
            struct Saved {
                #[state]
                gain: f64,
                #[state(nested, key = "filter")]
                child: FilterState,
            }
            "#,
        );
        assert!(
            expanded.contains("writer . begin_group (\"filter\")"),
            "{expanded}"
        );
        assert!(
            expanded.contains("self . child . save_state (writer) ?"),
            "{expanded}"
        );
        assert!(expanded.contains("writer . end_group ()"), "{expanded}");
        assert!(
            expanded.contains(
                "self . child . load_state_at (reader , & :: std :: format ! \
                 (\"{}{}/\" , prefix , \"filter\"))"
            ),
            "{expanded}"
        );
    }

    #[test]
    fn a_container_group_wraps_the_whole_struct() {
        let expanded = expand_str(
            r#"
            #[state(group = "dsp")]
            struct Saved {
                #[state]
                gain: f64,
            }
            "#,
        );
        assert!(
            expanded.contains("writer . begin_group (\"dsp\") ;"),
            "{expanded}"
        );
        assert!(expanded.contains("writer . end_group () ;"), "{expanded}");
        assert!(
            expanded.contains("self . load_state_at (reader , \"dsp/\")"),
            "{expanded}"
        );

        // With no group the root prefix is empty.
        let flat = expand_str("struct S { #[state] gain: f64 }");
        assert!(
            flat.contains("self . load_state_at (reader , \"\")"),
            "{flat}"
        );
        assert!(!flat.contains("begin_group"), "{flat}");
    }

    #[test]
    fn a_version_becomes_an_associated_constant() {
        let expanded = expand_str(
            r#"
            #[state(version = 3)]
            pub struct Saved {
                #[state]
                gain: f64,
            }
            "#,
        );
        assert!(
            expanded.contains(
                "pub const STATE_VERSION : :: daux_plugin :: __private :: StateVersion = \
                 :: daux_plugin :: __private :: StateVersion (3)"
            ),
            "{expanded}"
        );
        assert!(
            !expand_str(EVERY_TYPE).contains("STATE_VERSION"),
            "no version key"
        );
    }

    #[test]
    fn an_explicit_kind_overrides_the_written_type() {
        let expanded = expand_str(
            r#"
            struct Saved {
                #[state(kind = "bytes")]
                blob: MyBuffer,
                #[state(kind = "i64")]
                count: MyCount,
            }
            "#,
        );
        assert!(
            expanded.contains("put_bytes (\"blob\" , & self . blob)"),
            "{expanded}"
        );
        assert!(expanded.contains("put_i64 (\"count\""), "{expanded}");
    }

    #[test]
    fn an_empty_struct_still_gets_both_methods() {
        let expanded = expand_str("struct Empty {}");
        assert!(expanded.contains("fn save_state"), "{expanded}");
        assert!(expanded.contains("fn load_state"), "{expanded}");
        // Nothing is read, so the parameters must not warn in the author's crate.
        assert!(
            expanded.contains("# [allow (unused_variables ,"),
            "{expanded}"
        );
        // The author's crate must not be told off for the macro's own choices either.
        assert!(
            expanded.contains("clippy :: unnecessary_fallible_conversions"),
            "{expanded}"
        );
    }

    #[test]
    fn the_crate_path_can_be_redirected_and_generics_are_carried_through() {
        let expanded = expand_str(
            r#"
            #[state(crate = ::daux_plugin_api)]
            struct Saved<T: Dsp> {
                #[state]
                gain: f64,
                dsp: T,
            }
            "#,
        );
        assert!(
            expanded.contains("impl < T : Dsp > Saved < T >"),
            "{expanded}"
        );
        assert!(
            expanded.contains(":: daux_plugin_api :: __private :: StateWriter"),
            "{expanded}"
        );
    }

    #[test]
    fn every_read_goes_through_the_prefix() {
        // Regression: a read that used the bare key would ignore the group it lives in,
        // so a nested struct would silently read the root's values.
        let expanded = expand_str(
            r#"
            struct Saved {
                #[state]
                gain: f64,
                #[state]
                bypass: bool,
            }
            "#,
        );
        assert_eq!(expanded.matches("format !").count(), 2, "{expanded}");
        assert!(
            expanded.contains("format ! (\"{}{}\" , prefix , \"gain\")"),
            "{expanded}"
        );
        assert!(
            expanded.contains("format ! (\"{}{}\" , prefix , \"bypass\")"),
            "{expanded}"
        );
        assert!(!expanded.contains("reader . f64 (\"gain\")"), "{expanded}");
    }

    // ------------------------------------------------------------------- errors ---

    #[test]
    fn duplicate_keys_name_both_fields() {
        let message = error(
            r#"
            struct Saved {
                #[state(key = "gain")]
                a: f64,
                #[state(key = "gain")]
                b: f64,
            }
            "#,
        );
        assert!(
            message.contains("duplicate state key \"gain\" on `Saved`: field `a` already uses it"),
            "{message}"
        );
        assert!(
            message.contains("`a` first uses this key here"),
            "{message}"
        );

        // The default key is the field name, so this collides just as hard.
        let message = error(
            r#"
            struct Saved {
                #[state]
                gain: f64,
                #[state(key = "gain")]
                other: f64,
            }
            "#,
        );
        assert!(message.contains("duplicate state key"), "{message}");
    }

    #[test]
    fn a_malformed_key_is_rejected() {
        let message = error(r#"struct S { #[state(key = "a/b")] gain: f64 }"#);
        assert!(message.contains("may not contain `/`"), "{message}");

        let message = error(r#"struct S { #[state(key = "")] gain: f64 }"#);
        assert!(message.contains("may not be empty"), "{message}");

        let message = error(r#"#[state(group = "a/b")] struct S { #[state] gain: f64 }"#);
        assert!(message.contains("may not contain `/`"), "{message}");
    }

    #[test]
    fn an_unsupported_type_points_at_the_three_ways_out() {
        let message = error("struct S { #[state] table: Arc<Table> }");
        assert!(
            message.contains("cannot store `Arc < Table >`"),
            "{message}"
        );
        assert!(message.contains("kind = \"bytes\""), "{message}");
        assert!(message.contains("nested"), "{message}");
        assert!(message.contains("skip"), "{message}");

        // A `Vec` of anything but bytes is not a byte string.
        let message = error("struct S { #[state] curve: Vec<f32> }");
        assert!(message.contains("cannot store `Vec < f32 >`"), "{message}");
    }

    #[test]
    fn an_unknown_kind_lists_the_valid_ones() {
        let message = error(r#"struct S { #[state(kind = "u32")] count: MyCount }"#);
        assert!(message.contains("unknown storage kind `u32`"), "{message}");
        assert!(message.contains("f64, i64, bool, str, bytes"), "{message}");
    }

    #[test]
    fn skip_cannot_be_combined_with_other_keys() {
        let message = error(r#"struct S { #[state(skip, key = "gain")] gain: f64 }"#);
        assert!(
            message.contains("cannot be combined with `skip`"),
            "{message}"
        );

        let message = error("struct S { #[state(skip = 1)] gain: f64 }");
        assert!(message.contains("takes no value"), "{message}");
    }

    #[test]
    fn nested_rejects_the_keys_it_cannot_honour() {
        let message = error(r#"struct S { #[state(nested, kind = "bytes")] child: C }"#);
        assert!(message.contains("`kind` does not apply here"), "{message}");

        let message = error("struct S { #[state(nested, default)] child: C }");
        assert!(
            message.contains("`default` does not apply here"),
            "{message}"
        );
    }

    #[test]
    fn a_zero_version_is_rejected() {
        let message = error("#[state(version = 0)] struct S { #[state] gain: f64 }");
        assert!(message.contains("state version starts at 1"), "{message}");

        let message = error(r#"#[state(version = "2")] struct S { #[state] gain: f64 }"#);
        assert!(message.contains("must be an integer literal"), "{message}");
    }

    #[test]
    fn field_only_keys_are_rejected_on_the_container() {
        let message = error(r#"#[state(key = "dsp")] struct S { #[state] gain: f64 }"#);
        assert!(message.contains("unknown"), "{message}");
        assert!(
            message.contains("valid keys: version, group, crate"),
            "{message}"
        );
    }

    #[test]
    fn an_unknown_key_suggests_the_nearest() {
        let message = error(r#"struct S { #[state(keys = "gain")] gain: f64 }"#);
        assert!(
            message.contains("unknown `#[state(..)]` key `keys`"),
            "{message}"
        );
        assert!(message.contains("did you mean `key`?"), "{message}");
    }

    #[test]
    fn a_tuple_struct_or_enum_is_rejected_with_the_shape_to_write() {
        let message = error("struct S(f64);");
        assert!(message.contains("needs named fields"), "{message}");

        let message = error("enum S { A }");
        assert!(message.contains("struct with named fields"), "{message}");
    }
}

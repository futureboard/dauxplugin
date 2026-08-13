//! `#[derive(DauxPlugin)]` — the descriptor, and nothing else.
//!
//! A plug-in's audio behaviour is the one thing a macro must never invent, so this
//! derive stops at the static metadata a host can read without instantiating anything.
//! Everything it can check while the compiler is running, it checks: an id that
//! `PluginId::validate` would reject, a category nobody spells that way, a capability
//! that does not exist and an empty feature tag are all compile errors here rather than
//! a panic on the author's first run.

use proc_macro2::{Span, TokenStream};
use quote::{ToTokens, quote};
use syn::{Data, DeriveInput, Error, Expr, Ident, LitStr, Path, Result, Visibility};

use crate::attr::{self, AttrSet};

/// How the attribute is spelled, for diagnostics.
const CONTEXT: &str = "#[plugin(..)]";

/// Every key `#[plugin(..)]` accepts.
const KEYS: &[&str] = &[
    "id",
    "name",
    "vendor",
    "version",
    "description",
    "url",
    "support_url",
    "copyright",
    "license",
    "category",
    "capabilities",
    "features",
    "sample_formats",
    "state_schema_version",
    "min_abi",
    "crate",
];

/// Every category name accepted by `category = ".."`, and the `Category` variant it maps
/// to. The aliases mirror `daux_core::Category`'s `FromStr` so that a manifest and a
/// derive accept the same spellings.
const CATEGORIES: &[(&str, &str)] = &[
    ("effect", "Effect"),
    ("audio-effect", "Effect"),
    ("fx", "Effect"),
    ("instrument", "Instrument"),
    ("synth", "Instrument"),
    ("synthesizer", "Instrument"),
    ("synthesiser", "Instrument"),
    ("midi-effect", "MidiEffect"),
    ("note-effect", "MidiEffect"),
    ("event-effect", "MidiEffect"),
    ("analyzer", "Analyzer"),
    ("analyser", "Analyzer"),
    ("generator", "Generator"),
    ("tone-generator", "Generator"),
    ("utility", "Utility"),
    ("tool", "Utility"),
    ("unknown", "Unknown"),
];

/// The canonical category names, listed when a spelling is not recognised.
const CANONICAL_CATEGORIES: &[&str] = &[
    "effect",
    "instrument",
    "midi-effect",
    "analyzer",
    "generator",
    "utility",
    "unknown",
];

/// Every name accepted inside `capabilities(..)`, and the `Capabilities` constant it
/// maps to. Transcribed from `DAUX_CAP_*` in `docs/specifications/abi-v1.md` §6.2.
const CAPABILITIES: &[(&str, &str)] = &[
    ("audio_effect", "AUDIO_EFFECT"),
    ("instrument", "INSTRUMENT"),
    ("midi_effect", "MIDI_EFFECT"),
    ("analyzer", "ANALYZER"),
    ("midi_input", "MIDI_INPUT"),
    ("midi_output", "MIDI_OUTPUT"),
    ("midi2", "MIDI2"),
    ("sidechain", "SIDECHAIN"),
    ("dynamic_buses", "DYNAMIC_BUSES"),
    ("sample_accurate_auto", "SAMPLE_ACCURATE_AUTO"),
    ("note_expression", "NOTE_EXPRESSION"),
    ("has_gui", "HAS_GUI"),
    ("requires_gui", "REQUIRES_GUI"),
    ("shared_texture_gui", "SHARED_TEXTURE_GUI"),
    ("offline_render", "OFFLINE_RENDER"),
    ("hard_realtime", "HARD_REALTIME"),
    ("sandbox_safe", "SANDBOX_SAFE"),
    ("stereo_only", "STEREO_ONLY"),
    ("latency_dynamic", "LATENCY_DYNAMIC"),
    ("tail_infinite", "TAIL_INFINITE"),
];

/// The longest id the ABI can carry: `DauxId` is 128 bytes and the value is NUL-padded.
const MAX_ID_BYTES: usize = 127;

/// How `version = ..` was written.
enum VersionSpec {
    /// `version = "1.2.3"` or `"1.2.3.4"`, already split and range-checked.
    Parts {
        /// Major, minor, patch.
        parts: [u32; 3],
        /// The optional fourth component.
        build: Option<u32>,
        /// Where the literal was written.
        span: Span,
    },
    /// `version = MY_VERSION`, anything the builder can take an `Into<Version>` from.
    Expr(Expr),
}

/// How `category = ..` was written.
enum CategorySpec {
    /// `category = "instrument"`, resolved to a `Category` variant name.
    Known {
        /// The variant identifier, e.g. `Instrument`.
        variant: &'static str,
        /// Where the literal was written.
        span: Span,
    },
    /// `category = Category::Instrument`, passed straight through.
    Expr(Expr),
}

/// The whole derive input, after parsing and validation.
pub(crate) struct PluginInput {
    ident: Ident,
    vis: Visibility,
    generics: syn::Generics,
    krate: Path,
    id: LitStr,
    name: LitStr,
    vendor: Option<LitStr>,
    version: Option<VersionSpec>,
    description: Option<LitStr>,
    url: Option<LitStr>,
    support_url: Option<LitStr>,
    copyright: Option<LitStr>,
    license: Option<LitStr>,
    category: Option<CategorySpec>,
    capabilities: Option<Vec<Ident>>,
    features: Option<Vec<LitStr>>,
    sample_formats: Option<Vec<Ident>>,
    state_schema_version: Option<Expr>,
    min_abi: Option<(Expr, Expr)>,
}

/// Entry point: parse, validate, then generate.
pub(crate) fn derive(input: &DeriveInput) -> Result<TokenStream> {
    Ok(expand(&parse(input)?))
}

// ------------------------------------------------------------------------ parsing ---

/// Parses and fully validates a `#[derive(DauxPlugin)]` input.
pub(crate) fn parse(input: &DeriveInput) -> Result<PluginInput> {
    if !matches!(&input.data, Data::Struct(_)) {
        return Err(Error::new_spanned(
            &input.ident,
            "`#[derive(DauxPlugin)]` describes one plug-in, so it needs a struct\n  write \
             `#[plugin(id = \"com.example.gain\", name = \"Gain\")] struct Gain { .. }`",
        ));
    }

    let attrs = AttrSet::parse(&input.attrs, "plugin", CONTEXT, KEYS, input.ident.span())?;

    if !attrs.present || attrs.is_empty() {
        return Err(Error::new(
            input.ident.span(),
            "`#[derive(DauxPlugin)]` needs a `#[plugin(..)]` attribute\n  write \
             `#[plugin(id = \"com.example.gain\", name = \"Gain\")]`\n  the id is permanent: \
             changing it creates a different plug-in and orphans every saved project",
        ));
    }

    let Some(id_entry) = attrs.get("id") else {
        return Err(attrs.error(
            "`#[plugin(..)]` has no `id`\n  write `id = \"com.example.gain\"`, a reverse-DNS \
             name under a domain you own\n  it is permanent: renaming the product is free, \
             renumbering the id orphans every saved project",
        ));
    };
    let id = id_entry.str_value(CONTEXT)?;
    if let Err(reason) = validate_plugin_id(&id.value()) {
        return Err(Error::new(id.span(), reason));
    }

    let Some(name_entry) = attrs.get("name") else {
        return Err(attrs.error(
            "`#[plugin(..)]` has no `name`\n  write `name = \"Gain\"`, the product name a \
             host shows in its browser",
        ));
    };
    let name = name_entry.str_value(CONTEXT)?;
    if name.value().trim().is_empty() {
        return Err(Error::new(
            name.span(),
            "a plug-in name may not be blank: it is what the user sees in the browser",
        ));
    }

    Ok(PluginInput {
        ident: input.ident.clone(),
        vis: input.vis.clone(),
        generics: input.generics.clone(),
        krate: attrs.crate_path()?,
        id,
        name,
        vendor: optional_str(&attrs, "vendor")?,
        version: parse_version(&attrs)?,
        description: optional_str(&attrs, "description")?,
        url: optional_str(&attrs, "url")?,
        support_url: optional_str(&attrs, "support_url")?,
        copyright: optional_str(&attrs, "copyright")?,
        license: optional_str(&attrs, "license")?,
        category: parse_category(&attrs)?,
        capabilities: parse_named_list(&attrs, "capabilities", CAPABILITIES, "capability")?,
        features: parse_features(&attrs)?,
        sample_formats: parse_sample_formats(&attrs)?,
        state_schema_version: parse_state_schema_version(&attrs)?,
        min_abi: parse_min_abi(&attrs)?,
    })
}

/// Reads an optional `key = "…"` entry.
fn optional_str(attrs: &AttrSet, key: &str) -> Result<Option<LitStr>> {
    match attrs.get(key) {
        Some(entry) => Ok(Some(entry.str_value(CONTEXT)?)),
        None => Ok(None),
    }
}

/// Transcription of `daux_core::PluginId::validate`, so that an unusable id is a compile
/// error instead of a panic inside the generated `descriptor()`.
///
/// The rules are part of the ABI (`abi-v1` §5) and are duplicated here on purpose: a
/// proc-macro crate cannot depend on `daux-core` without dragging the whole model into
/// every build script that uses the derive.
fn validate_plugin_id(id: &str) -> std::result::Result<(), String> {
    let advice = "\n  a plug-in id is reverse-DNS, lower-case ASCII, at most 127 bytes: \
                  `com.example.gain`";
    if id.is_empty() {
        return Err(format!("a plug-in id may not be empty{advice}"));
    }
    if id.len() > MAX_ID_BYTES {
        return Err(format!(
            "this plug-in id is {} bytes, the ABI limit is {MAX_ID_BYTES}{advice}",
            id.len()
        ));
    }
    if !id.contains('.') {
        return Err(format!(
            "plug-in id `{id}` has no `.`; reverse-DNS needs at least two labels{advice}"
        ));
    }
    for label in id.split('.') {
        if label.is_empty() {
            return Err(format!(
                "plug-in id `{id}` has an empty label: no leading or trailing `.`, and no \
                 `..`{advice}"
            ));
        }
        let mut bytes = label.bytes();
        // `split` never yields an empty label here, so the first byte exists.
        let first = bytes.next().unwrap_or(b'\0');
        if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
            return Err(format!(
                "plug-in id label `{label}` must start with a lower-case ASCII letter or a \
                 digit{advice}"
            ));
        }
        for byte in bytes {
            if !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && byte != b'-' && byte != b'_'
            {
                return Err(format!(
                    "plug-in id label `{label}` contains `{}`; only lower-case ASCII letters, \
                     digits, `-` and `_` are allowed{advice}",
                    char::from(byte).escape_debug()
                ));
            }
        }
    }
    if !id.bytes().any(|b| b.is_ascii_lowercase()) {
        return Err(format!(
            "plug-in id `{id}` contains no ASCII letter{advice}"
        ));
    }
    Ok(())
}

/// Parses `version = "1.2.3"`, `version = "1.2.3.4"` or an arbitrary expression.
fn parse_version(attrs: &AttrSet) -> Result<Option<VersionSpec>> {
    let Some(entry) = attrs.get("version") else {
        return Ok(None);
    };
    let expr = entry.expr(CONTEXT)?;
    let Ok(lit) = entry.str_value(CONTEXT) else {
        return Ok(Some(VersionSpec::Expr(expr.clone())));
    };

    let text = lit.value();
    let mut numbers = Vec::new();
    for component in text.trim().split('.') {
        let Ok(value) = component.parse::<u32>() else {
            return Err(Error::new(
                lit.span(),
                format!(
                    "`{text}` is not a version\n  write `version = \"1.2.3\"` or \
                     `\"1.2.3.4\"`, every component a number that fits in a `u32`"
                ),
            ));
        };
        numbers.push(value);
    }
    let (parts, build) = match numbers[..] {
        [major, minor, patch] => ([major, minor, patch], None),
        [major, minor, patch, build] => ([major, minor, patch], Some(build)),
        _ => {
            return Err(Error::new(
                lit.span(),
                format!(
                    "`{text}` has {} components\n  a version is `major.minor.patch` with an \
                     optional build: `version = \"1.2.3\"` or `\"1.2.3.4\"`",
                    numbers.len()
                ),
            ));
        }
    };
    Ok(Some(VersionSpec::Parts {
        parts,
        build,
        span: lit.span(),
    }))
}

/// Parses `category = "instrument"` or `category = Category::Instrument`.
fn parse_category(attrs: &AttrSet) -> Result<Option<CategorySpec>> {
    let Some(entry) = attrs.get("category") else {
        return Ok(None);
    };
    let expr = entry.expr(CONTEXT)?;
    let Ok(lit) = entry.str_value(CONTEXT) else {
        return Ok(Some(CategorySpec::Expr(expr.clone())));
    };

    let written = lit.value();
    let normalised = written.trim().to_ascii_lowercase().replace(['_', ' '], "-");
    let Some((_, variant)) = CATEGORIES.iter().find(|(name, _)| *name == normalised) else {
        let mut message = format!(
            "unknown plug-in category `{written}`\n  valid categories: {}",
            CANONICAL_CATEGORIES.join(", ")
        );
        if let Some(closest) = attr::suggest(&normalised, CANONICAL_CATEGORIES) {
            message.push_str(&format!("\n  did you mean `{closest}`?"));
        }
        return Err(Error::new(lit.span(), message));
    };
    Ok(Some(CategorySpec::Known {
        variant,
        span: lit.span(),
    }))
}

/// Parses a parenthesised list of bare names against a table of accepted ones.
fn parse_named_list(
    attrs: &AttrSet,
    key: &str,
    table: &[(&str, &str)],
    what: &str,
) -> Result<Option<Vec<Ident>>> {
    let Some(entry) = attrs.get(key) else {
        return Ok(None);
    };
    let names = entry.ident_list(CONTEXT)?;
    for name in &names {
        let written = name.to_string();
        if !table.iter().any(|(accepted, _)| *accepted == written) {
            let valid: Vec<&str> = table.iter().map(|(accepted, _)| *accepted).collect();
            let mut message = format!("unknown {what} `{written}`\n  valid: {}", valid.join(", "));
            if let Some(closest) = attr::suggest(&written, &valid) {
                message.push_str(&format!("\n  did you mean `{closest}`?"));
            }
            return Err(Error::new(name.span(), message));
        }
    }
    for (index, name) in names.iter().enumerate() {
        if let Some(earlier) = names[..index].iter().find(|other| *other == name) {
            let mut error = Error::new(name.span(), format!("`{name}` is listed twice"));
            error.combine(Error::new(
                earlier.span(),
                format!("`{name}` is first listed here"),
            ));
            return Err(error);
        }
    }
    Ok(Some(names))
}

/// Parses `features("reverb", "stereo")`.
fn parse_features(attrs: &AttrSet) -> Result<Option<Vec<LitStr>>> {
    let Some(entry) = attrs.get("features") else {
        return Ok(None);
    };
    let mut tags = Vec::new();
    for item in entry.list(CONTEXT)? {
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(lit),
            ..
        }) = item
        else {
            return Err(Error::new_spanned(
                item,
                "`features` takes string literals\n  write `features(\"reverb\", \"stereo\")`",
            ));
        };
        if lit.value().trim().is_empty() {
            return Err(Error::new(
                lit.span(),
                "a feature tag may not be blank: hosts index these, and an empty tag is \
                 rejected by `PluginDescriptor::validate`",
            ));
        }
        tags.push(lit.clone());
    }
    Ok(Some(tags))
}

/// Parses `sample_formats(f32, f64)`.
fn parse_sample_formats(attrs: &AttrSet) -> Result<Option<Vec<Ident>>> {
    const FORMATS: &[(&str, &str)] = &[("f32", "F32"), ("f64", "F64")];
    let Some(formats) = parse_named_list(attrs, "sample_formats", FORMATS, "sample format")? else {
        return Ok(None);
    };
    let entry = attrs
        .get("sample_formats")
        .unwrap_or_else(|| unreachable!("the key was just read"));
    if formats.is_empty() {
        return Err(Error::new(
            entry.span(),
            "`sample_formats()` is empty\n  every DAUx plug-in processes `f32`; write \
             `sample_formats(f32)` or `sample_formats(f32, f64)`",
        ));
    }
    if !formats.iter().any(|format| format == "f32") {
        return Err(Error::new(
            entry.span(),
            "`f32` is missing\n  abi-v1 §8 requires every plug-in to process `f32`; `f64` is \
             an addition, never a replacement",
        ));
    }
    Ok(Some(formats))
}

/// Parses `state_schema_version = 3`.
fn parse_state_schema_version(attrs: &AttrSet) -> Result<Option<Expr>> {
    let Some(entry) = attrs.get("state_schema_version") else {
        return Ok(None);
    };
    let expr = entry.expr(CONTEXT)?;
    if let Some(value) = attr::numeric_literal(expr) {
        if value < 1.0 || value.fract() != 0.0 {
            return Err(Error::new_spanned(
                expr,
                "a state schema version is a whole number starting at 1\n  bump it when the \
                 meaning of a saved field changes, never when a field is merely renamed",
            ));
        }
    }
    Ok(Some(expr.clone()))
}

/// Parses `min_abi = (1, 0)`.
fn parse_min_abi(attrs: &AttrSet) -> Result<Option<(Expr, Expr)>> {
    let Some(entry) = attrs.get("min_abi") else {
        return Ok(None);
    };
    let expr = entry.expr(CONTEXT)?;
    let syn::Expr::Tuple(tuple) = expr else {
        return Err(Error::new_spanned(
            expr,
            "`min_abi` is a `(major, minor)` pair\n  write `min_abi = (1, 0)`",
        ));
    };
    let elements: Vec<Expr> = tuple.elems.iter().cloned().collect();
    let [major, minor] = &elements[..] else {
        return Err(Error::new_spanned(
            expr,
            "`min_abi` is a `(major, minor)` pair\n  write `min_abi = (1, 0)`",
        ));
    };
    if attr::numeric_literal(major) == Some(0.0) {
        return Err(Error::new_spanned(
            major,
            "ABI major 0 does not exist: v1 is the first release\n  write `min_abi = (1, 0)`",
        ));
    }
    Ok(Some((major.clone(), minor.clone())))
}

// --------------------------------------------------------------------- generation ---

/// Generates the inherent `descriptor()` and nothing else.
fn expand(input: &PluginInput) -> TokenStream {
    let PluginInput {
        ident,
        vis,
        generics,
        krate,
        id,
        name,
        ..
    } = input;
    let private = quote!(#krate::__private);
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let mut calls: Vec<TokenStream> = Vec::new();
    if let Some(vendor) = &input.vendor {
        calls.push(quote!(.vendor(#vendor)));
    }
    if let Some(version) = &input.version {
        let expr = version_expr(&private, version);
        calls.push(quote!(.version(#expr)));
    }
    if let Some(description) = &input.description {
        calls.push(quote!(.description(#description)));
    }
    if let Some(url) = &input.url {
        calls.push(quote!(.url(#url)));
    }
    if let Some(support_url) = &input.support_url {
        calls.push(quote!(.support_url(#support_url)));
    }
    if let Some(copyright) = &input.copyright {
        calls.push(quote!(.copyright(#copyright)));
    }
    if let Some(license) = &input.license {
        calls.push(quote!(.license(#license)));
    }
    if let Some(category) = &input.category {
        let expr = category_expr(&private, category);
        calls.push(quote!(.category(#expr)));
    }
    if let Some(capabilities) = &input.capabilities {
        let expr = capabilities_expr(&private, capabilities);
        calls.push(quote!(.capabilities(#expr)));
    }
    if let Some(features) = &input.features {
        calls.push(quote!(.features([ #(#features),* ])));
    }
    if let Some(formats) = &input.sample_formats {
        let expr = sample_formats_expr(&private, formats);
        calls.push(quote!(.sample_formats(#expr)));
    }
    if let Some(version) = &input.state_schema_version {
        calls.push(quote!(.state_schema_version(#version)));
    }
    if let Some((major, minor)) = &input.min_abi {
        calls.push(quote!(.min_abi(#major, #minor)));
    }

    let doc = format!(
        "`[main-thread]` The static description of `{}`, as declared by `#[plugin(..)]`.\
         \n\nGenerated by `#[derive(DauxPlugin)]`. Delegate to it from the hand-written \
         `DauxPlugin::descriptor`; the derive never generates DSP.\n\n# Panics\n\nNever, for \
         a descriptor this macro accepted: the id, the name, the feature tags, the sample \
         formats and the minimum ABI are all validated while the macro runs. It can only \
         panic if a key was given a constant expression the macro could not evaluate and \
         that constant is invalid.",
        ident,
    );
    let failure = format!(
        "the descriptor of `{}` is invalid; `#[plugin(..)]` accepted it, so a key must \
         carry a constant this macro could not check",
        id.value(),
    );

    quote! {
        #[automatically_derived]
        impl #impl_generics #ident #ty_generics #where_clause {
            #[doc = #doc]
            #[must_use]
            #vis fn descriptor() -> #private::PluginDescriptor {
                #private::PluginDescriptor::builder(#id, #name)
                    #(#calls)*
                    .build()
                    .expect(#failure)
            }
        }
    }
}

/// The `Version` expression for `version = ..`.
fn version_expr(private: &TokenStream, version: &VersionSpec) -> TokenStream {
    match version {
        VersionSpec::Parts { parts, build, span } => {
            let [major, minor, patch] = *parts;
            let (major, minor, patch) = (
                syn::LitInt::new(&format!("{major}u32"), *span),
                syn::LitInt::new(&format!("{minor}u32"), *span),
                syn::LitInt::new(&format!("{patch}u32"), *span),
            );
            let base = quote!(#private::Version::new(#major, #minor, #patch));
            match build {
                Some(build) => {
                    let build = syn::LitInt::new(&format!("{build}u32"), *span);
                    quote!(#base.with_build(#build))
                }
                None => base,
            }
        }
        VersionSpec::Expr(expr) => expr.to_token_stream(),
    }
}

/// The `Category` expression for `category = ..`.
fn category_expr(private: &TokenStream, category: &CategorySpec) -> TokenStream {
    match category {
        CategorySpec::Known { variant, span } => {
            let variant = Ident::new(variant, *span);
            quote!(#private::Category::#variant)
        }
        CategorySpec::Expr(expr) => expr.to_token_stream(),
    }
}

/// The `Capabilities` expression for `capabilities(..)`.
fn capabilities_expr(private: &TokenStream, names: &[Ident]) -> TokenStream {
    if names.is_empty() {
        return quote!(#private::Capabilities::NONE);
    }
    let constants = names.iter().map(|name| {
        let constant = lookup(CAPABILITIES, name);
        quote!(#private::Capabilities::#constant)
    });
    quote!(#(#constants)|*)
}

/// The `SampleFormats` expression for `sample_formats(..)`.
fn sample_formats_expr(private: &TokenStream, names: &[Ident]) -> TokenStream {
    const FORMATS: &[(&str, &str)] = &[("f32", "F32"), ("f64", "F64")];
    if names.len() == 2 {
        return quote!(#private::SampleFormats::BOTH);
    }
    let constant = names.first().map_or_else(
        || Ident::new("F32", Span::call_site()),
        |name| lookup(FORMATS, name),
    );
    quote!(#private::SampleFormats::#constant)
}

/// Looks a validated name up in one of the tables, keeping the caller's span.
fn lookup(table: &[(&str, &str)], name: &Ident) -> Ident {
    let written = name.to_string();
    let constant = table
        .iter()
        .find(|(accepted, _)| *accepted == written)
        .map_or_else(
            || unreachable!("names were validated during parsing"),
            |(_, constant)| *constant,
        );
    Ident::new(constant, name.span())
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

    const GAIN: &str = r#"
        #[plugin(id = "com.example.gain", name = "Gain", vendor = "Example Audio",
                 version = "1.2.3", description = "A gain.", category = "effect",
                 capabilities(audio_effect, has_gui), features("utility", "gain"))]
        pub struct Gain { inner: u32 }
    "#;

    #[test]
    fn generates_the_builder_chain_in_order() {
        let expanded = expand_str(GAIN);
        assert!(
            expanded.contains(
                ":: daux_plugin :: __private :: PluginDescriptor :: builder \
                 (\"com.example.gain\" , \"Gain\")"
            ),
            "{expanded}"
        );
        assert!(
            expanded.contains(". vendor (\"Example Audio\")"),
            "{expanded}"
        );
        assert!(
            expanded.contains("Version :: new (1u32 , 2u32 , 3u32)"),
            "{expanded}"
        );
        assert!(expanded.contains("Category :: Effect"), "{expanded}");
        assert!(
            expanded.contains(
                "Capabilities :: AUDIO_EFFECT | :: daux_plugin :: __private :: Capabilities :: \
                 HAS_GUI"
            ),
            "{expanded}"
        );
        assert!(
            expanded.contains(". features ([\"utility\" , \"gain\"])"),
            "{expanded}"
        );
        assert!(expanded.contains("pub fn descriptor ()"), "{expanded}");
    }

    #[test]
    fn it_generates_a_descriptor_and_never_dsp() {
        let expanded = expand_str(GAIN);
        // The single most important property of this derive: it must not invent audio
        // behaviour, a trait impl or any state handling.
        assert!(!expanded.contains("fn process"), "{expanded}");
        assert!(!expanded.contains("impl :: daux_plugin"), "{expanded}");
        assert!(!expanded.contains("DauxPlugin for"), "{expanded}");
        assert!(!expanded.contains("DauxProcessor"), "{expanded}");
        assert!(!expanded.contains("DauxController"), "{expanded}");
        assert!(!expanded.contains("save_state"), "{expanded}");
        // Exactly one generated item.
        assert_eq!(expanded.matches("fn ").count(), 1, "{expanded}");
    }

    #[test]
    fn a_minimal_declaration_omits_every_optional_call() {
        let expanded = expand_str(
            r#"
            #[plugin(id = "com.example.x", name = "X")]
            struct X;
            "#,
        );
        assert!(!expanded.contains(". vendor"), "{expanded}");
        assert!(!expanded.contains(". category"), "{expanded}");
        assert!(!expanded.contains(". features"), "{expanded}");
        assert!(!expanded.contains(". capabilities"), "{expanded}");
        assert!(expanded.contains(". build () . expect"), "{expanded}");
        // A private struct keeps a private descriptor.
        assert!(!expanded.contains("pub fn descriptor"), "{expanded}");
    }

    #[test]
    fn a_four_part_version_carries_its_build_number() {
        let expanded = expand_str(
            r#"
            #[plugin(id = "com.example.x", name = "X", version = "1.2.3.400")]
            struct X;
            "#,
        );
        assert!(
            expanded.contains("Version :: new (1u32 , 2u32 , 3u32) . with_build (400u32)"),
            "{expanded}"
        );
    }

    #[test]
    fn non_literal_values_are_passed_straight_through() {
        let expanded = expand_str(
            r#"
            #[plugin(id = "com.example.x", name = "X", version = MY_VERSION,
                     category = Category::Instrument, state_schema_version = SCHEMA,
                     min_abi = (1, 2))]
            struct X;
            "#,
        );
        assert!(expanded.contains(". version (MY_VERSION)"), "{expanded}");
        assert!(
            expanded.contains(". category (Category :: Instrument)"),
            "{expanded}"
        );
        assert!(
            expanded.contains(". state_schema_version (SCHEMA)"),
            "{expanded}"
        );
        assert!(expanded.contains(". min_abi (1 , 2)"), "{expanded}");
    }

    #[test]
    fn the_crate_path_can_be_redirected_and_generics_are_carried_through() {
        let expanded = expand_str(
            r#"
            #[plugin(id = "com.example.x", name = "X", crate = ::daux_plugin_api)]
            struct X<T: Dsp> { dsp: T }
            "#,
        );
        assert!(expanded.contains("impl < T : Dsp > X < T >"), "{expanded}");
        assert!(
            expanded.contains(":: daux_plugin_api :: __private :: PluginDescriptor"),
            "{expanded}"
        );
    }

    #[test]
    fn sample_formats_collapse_to_the_right_constant() {
        let expanded = expand_str(
            r#"
            #[plugin(id = "com.example.x", name = "X", sample_formats(f32))]
            struct X;
            "#,
        );
        assert!(expanded.contains("SampleFormats :: F32"), "{expanded}");

        let expanded = expand_str(
            r#"
            #[plugin(id = "com.example.x", name = "X", sample_formats(f32, f64))]
            struct X;
            "#,
        );
        assert!(expanded.contains("SampleFormats :: BOTH"), "{expanded}");
    }

    #[test]
    fn category_aliases_map_to_the_canonical_variant() {
        for (written, variant) in [
            ("synth", "Instrument"),
            ("MIDI_Effect", "MidiEffect"),
            (" analyser ", "Analyzer"),
            ("fx", "Effect"),
            ("tool", "Utility"),
        ] {
            let source = format!(
                r#"#[plugin(id = "com.example.x", name = "X", category = "{written}")] struct X;"#
            );
            let expanded = expand_str(&source);
            assert!(
                expanded.contains(&format!("Category :: {variant}")),
                "`{written}` should map to {variant}: {expanded}"
            );
        }
    }

    // ------------------------------------------------------------------- errors ---

    #[test]
    fn a_missing_attribute_or_key_says_what_to_write() {
        let message = error("struct Gain;");
        assert!(
            message.contains("needs a `#[plugin(..)]` attribute"),
            "{message}"
        );

        let message = error(r#"#[plugin(name = "Gain")] struct Gain;"#);
        assert!(message.contains("has no `id`"), "{message}");
        assert!(message.contains("reverse-DNS"), "{message}");

        let message = error(r#"#[plugin(id = "com.example.gain")] struct Gain;"#);
        assert!(message.contains("has no `name`"), "{message}");
    }

    #[test]
    fn every_plugin_id_rule_is_enforced_at_compile_time() {
        let cases = [
            ("Gain", "no `.`"),
            ("com.Example.gain", "must start with a lower-case"),
            ("com..gain", "empty label"),
            ("com.example.gain!", "only lower-case ASCII letters"),
            ("com.-gain", "must start with a lower-case"),
            ("", "may not be empty"),
            ("1.2", "no ASCII letter"),
        ];
        for (id, expected) in cases {
            let source = format!(r#"#[plugin(id = "{id}", name = "X")] struct X;"#);
            let message = error(&source);
            assert!(
                message.contains(expected),
                "id `{id}` should be rejected with `{expected}`, got: {message}"
            );
        }

        let long = "com.".to_owned() + &"a".repeat(125);
        let source = format!(r#"#[plugin(id = "{long}", name = "X")] struct X;"#);
        let message = error(&source);
        assert!(message.contains("ABI limit is 127"), "{message}");
    }

    #[test]
    fn a_valid_but_unusual_id_is_accepted() {
        // Digits, dashes, underscores and long chains are all legal; the derive must not
        // be stricter than `PluginId::validate`.
        let expanded = expand_str(
            r#"
            #[plugin(id = "studio.futureboard.eq-3_band.v2", name = "EQ")]
            struct Eq;
            "#,
        );
        assert!(
            expanded.contains("studio.futureboard.eq-3_band.v2"),
            "{expanded}"
        );
    }

    #[test]
    fn a_blank_name_is_rejected() {
        let message = error(r#"#[plugin(id = "com.example.x", name = "   ")] struct X;"#);
        assert!(message.contains("may not be blank"), "{message}");
    }

    #[test]
    fn an_unknown_category_suggests_the_nearest() {
        let message =
            error(r#"#[plugin(id = "com.example.x", name = "X", category = "efect")] struct X;"#);
        assert!(
            message.contains("unknown plug-in category `efect`"),
            "{message}"
        );
        assert!(message.contains("did you mean `effect`?"), "{message}");
    }

    #[test]
    fn an_unknown_capability_suggests_the_nearest() {
        let message = error(
            r#"#[plugin(id = "com.example.x", name = "X", capabilities(has_qui))] struct X;"#,
        );
        assert!(
            message.contains("unknown capability `has_qui`"),
            "{message}"
        );
        assert!(message.contains("did you mean `has_gui`?"), "{message}");
    }

    #[test]
    fn a_repeated_capability_names_both_occurrences() {
        let message = error(
            r#"#[plugin(id = "com.example.x", name = "X", capabilities(has_gui, has_gui))]
               struct X;"#,
        );
        assert!(message.contains("`has_gui` is listed twice"), "{message}");
        assert!(message.contains("first listed here"), "{message}");
    }

    #[test]
    fn a_malformed_version_is_rejected() {
        for (version, expected) in [
            ("1.2", "components"),
            ("1.2.3.4.5", "components"),
            ("1.2.x", "is not a version"),
            ("99999999999.0.0", "is not a version"),
            ("-1.0.0", "is not a version"),
        ] {
            let source = format!(
                r#"#[plugin(id = "com.example.x", name = "X", version = "{version}")] struct X;"#
            );
            let message = error(&source);
            assert!(
                message.contains(expected),
                "`{version}` should be rejected with `{expected}`, got: {message}"
            );
        }
    }

    #[test]
    fn a_blank_or_non_literal_feature_tag_is_rejected() {
        let message = error(
            r#"#[plugin(id = "com.example.x", name = "X", features("reverb", " "))] struct X;"#,
        );
        assert!(message.contains("may not be blank"), "{message}");

        let message =
            error(r#"#[plugin(id = "com.example.x", name = "X", features(reverb))] struct X;"#);
        assert!(message.contains("takes string literals"), "{message}");
    }

    #[test]
    fn f32_support_cannot_be_dropped() {
        let message =
            error(r#"#[plugin(id = "com.example.x", name = "X", sample_formats(f64))] struct X;"#);
        assert!(message.contains("`f32` is missing"), "{message}");

        let message =
            error(r#"#[plugin(id = "com.example.x", name = "X", sample_formats())] struct X;"#);
        assert!(message.contains("is empty"), "{message}");

        let message =
            error(r#"#[plugin(id = "com.example.x", name = "X", sample_formats(i16))] struct X;"#);
        assert!(message.contains("unknown sample format `i16`"), "{message}");
    }

    #[test]
    fn abi_major_zero_and_a_malformed_pair_are_rejected() {
        let message =
            error(r#"#[plugin(id = "com.example.x", name = "X", min_abi = (0, 1))] struct X;"#);
        assert!(message.contains("ABI major 0 does not exist"), "{message}");

        let message =
            error(r#"#[plugin(id = "com.example.x", name = "X", min_abi = 1)] struct X;"#);
        assert!(message.contains("`(major, minor)` pair"), "{message}");

        let message =
            error(r#"#[plugin(id = "com.example.x", name = "X", min_abi = (1, 0, 0))] struct X;"#);
        assert!(message.contains("`(major, minor)` pair"), "{message}");
    }

    #[test]
    fn a_state_schema_version_below_one_is_rejected() {
        let message = error(
            r#"#[plugin(id = "com.example.x", name = "X", state_schema_version = 0)] struct X;"#,
        );
        assert!(message.contains("whole number starting at 1"), "{message}");
    }

    #[test]
    fn an_unknown_key_lists_the_valid_ones() {
        let message =
            error(r#"#[plugin(id = "com.example.x", name = "X", vendour = "Y")] struct X;"#);
        assert!(
            message.contains("unknown `#[plugin(..)]` key `vendour`"),
            "{message}"
        );
        assert!(message.contains("did you mean `vendor`?"), "{message}");
    }

    #[test]
    fn keys_written_in_the_wrong_shape_name_the_right_one() {
        let message = error(
            r#"#[plugin(id = "com.example.x", name = "X", capabilities = has_gui)] struct X;"#,
        );
        assert!(message.contains("needs a parenthesised list"), "{message}");

        let message =
            error(r#"#[plugin(id = "com.example.x", name = "X", features = "reverb")] struct X;"#);
        assert!(message.contains("needs a parenthesised list"), "{message}");

        let message = error(r#"#[plugin(id = 7, name = "X")] struct X;"#);
        assert!(message.contains("must be a string literal"), "{message}");

        let message = error(r#"#[plugin(id, name = "X")] struct X;"#);
        assert!(message.contains("`id` needs a value"), "{message}");
    }

    #[test]
    fn an_enum_or_union_is_rejected() {
        let message = error(r#"#[plugin(id = "com.example.x", name = "X")] enum X { A }"#);
        assert!(message.contains("needs a struct"), "{message}");
    }

    #[test]
    fn an_empty_attribute_is_rejected() {
        let message = error("#[plugin()] struct X;");
        assert!(
            message.contains("needs a `#[plugin(..)]` attribute"),
            "{message}"
        );
    }
}

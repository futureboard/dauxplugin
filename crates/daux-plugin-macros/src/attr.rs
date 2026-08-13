//! Attribute parsing shared by the three derives.
//!
//! Everything here is pure parsing and validation with no token generation, so the
//! whole surface can be exercised by the unit tests at the bottom of each module. A
//! proc-macro crate cannot compile its own output, so the parts that *can* be tested
//! directly are deliberately kept separate from the parts that cannot.
//!
//! The model is intentionally tiny: an attribute is a comma-separated list of entries,
//! and every entry is one of
//!
//! ```text
//! skip                          // a bare flag
//! id = 1                        // a key with a value expression
//! flags(automatable, hidden)    // a key with a parenthesised list
//! ```

use proc_macro2::Span;
use quote::ToTokens;
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    Attribute, Error, Expr, ExprLit, ExprUnary, Ident, Lit, LitFloat, LitInt, LitStr, Path, Result,
    Token, UnOp, parenthesized,
};

/// The value part of one attribute entry.
#[derive(Debug)]
pub(crate) enum AttrValue {
    /// `skip` — the key stands alone.
    Flag,
    /// `id = 1` — the key carries one expression.
    Value(Expr),
    /// `flags(automatable, hidden)` — the key carries a parenthesised list.
    List(Vec<Expr>),
}

/// One `key`, `key = value` or `key(a, b)` entry of an attribute.
#[derive(Debug)]
pub(crate) struct AttrEntry {
    /// The key. Parsed with `parse_any` so that keywords such as `crate` are accepted.
    pub(crate) key: Ident,
    /// What followed the key.
    pub(crate) value: AttrValue,
}

impl Parse for AttrEntry {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let key = Ident::parse_any(input)?;
        let value = if input.peek(Token![=]) {
            let _eq: Token![=] = input.parse()?;
            AttrValue::Value(input.parse()?)
        } else if input.peek(syn::token::Paren) {
            let inner;
            parenthesized!(inner in input);
            let items = Punctuated::<Expr, Token![,]>::parse_terminated(&inner)?;
            AttrValue::List(items.into_iter().collect())
        } else {
            AttrValue::Flag
        };
        Ok(Self { key, value })
    }
}

impl AttrEntry {
    /// The key as a string.
    pub(crate) fn name(&self) -> String {
        self.key.unraw().to_string()
    }

    /// The span of the key, used for "you wrote this" diagnostics.
    pub(crate) fn span(&self) -> Span {
        self.key.span()
    }

    /// `true` when the key stands alone, with neither `= value` nor `(list)`.
    pub(crate) fn is_flag(&self) -> bool {
        matches!(self.value, AttrValue::Flag)
    }

    /// The expression after `=`, or an error naming the syntax that was expected.
    pub(crate) fn expr(&self, context: &str) -> Result<&Expr> {
        match &self.value {
            AttrValue::Value(expr) => Ok(expr),
            _ => Err(Error::new(
                self.span(),
                format!(
                    "`{key}` needs a value\n  write `{context}: {key} = ...`",
                    key = self.name(),
                ),
            )),
        }
    }

    /// The string literal after `=`.
    pub(crate) fn str_value(&self, context: &str) -> Result<LitStr> {
        let expr = self.expr(context)?;
        match expr {
            Expr::Lit(ExprLit {
                lit: Lit::Str(lit), ..
            }) => Ok(lit.clone()),
            other => Err(Error::new_spanned(
                other,
                format!(
                    "`{key}` must be a string literal\n  write `{key} = \"...\"`",
                    key = self.name(),
                ),
            )),
        }
    }

    /// The integer literal after `=`.
    pub(crate) fn int_value(&self, context: &str) -> Result<LitInt> {
        let expr = self.expr(context)?;
        match expr {
            Expr::Lit(ExprLit {
                lit: Lit::Int(lit), ..
            }) => Ok(lit.clone()),
            other => Err(Error::new_spanned(
                other,
                format!(
                    "`{key}` must be an integer literal\n  write `{key} = 1`",
                    key = self.name(),
                ),
            )),
        }
    }

    /// The path after `=`, e.g. `crate = ::daux_plugin`.
    pub(crate) fn path_value(&self, context: &str) -> Result<Path> {
        let expr = self.expr(context)?;
        match expr {
            Expr::Path(path) if path.qself.is_none() && path.attrs.is_empty() => {
                Ok(path.path.clone())
            }
            other => Err(Error::new_spanned(
                other,
                format!(
                    "`{key}` must be a path\n  write `{key} = ::daux_plugin`",
                    key = self.name(),
                ),
            )),
        }
    }

    /// The parenthesised list, or an error naming the syntax that was expected.
    pub(crate) fn list(&self, _context: &str) -> Result<&[Expr]> {
        match &self.value {
            AttrValue::List(items) => Ok(items),
            _ => Err(Error::new(
                self.span(),
                format!(
                    "`{key}` needs a parenthesised list\n  write `{key}(first, second)`",
                    key = self.name(),
                ),
            )),
        }
    }

    /// The parenthesised list, with every element required to be a bare name.
    pub(crate) fn ident_list(&self, context: &str) -> Result<Vec<Ident>> {
        self.list(context)?
            .iter()
            .map(|expr| match expr {
                Expr::Path(path)
                    if path.qself.is_none()
                        && path.attrs.is_empty()
                        && path.path.get_ident().is_some() =>
                {
                    Ok(path
                        .path
                        .get_ident()
                        .cloned()
                        .unwrap_or_else(|| unreachable!("get_ident() was just checked to be Some")))
                }
                other => Err(Error::new_spanned(
                    other,
                    format!(
                        "`{key}` takes bare names\n  write `{key}(first_name, second_name)`",
                        key = self.name(),
                    ),
                )),
            })
            .collect()
    }

    /// Rejects a value on a key that only exists as a flag.
    pub(crate) fn expect_flag(&self, context: &str) -> Result<()> {
        match &self.value {
            AttrValue::Flag => Ok(()),
            _ => Err(Error::new(
                self.span(),
                format!(
                    "`{key}` takes no value\n  write `{context}: {key}`",
                    key = self.name(),
                ),
            )),
        }
    }
}

/// Every entry of one attribute kind on one item, already validated against the set of
/// keys that item accepts.
#[derive(Debug)]
pub(crate) struct AttrSet {
    /// How the attribute is written, for diagnostics — e.g. `#[param(..)]`.
    pub(crate) context: &'static str,
    /// Span to blame when a required key is missing.
    pub(crate) span: Span,
    /// `true` when the attribute appeared at all, even with no entries.
    pub(crate) present: bool,
    entries: Vec<AttrEntry>,
}

/// Whether a bare `#[foo]` with no arguments means anything for this item.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bare {
    /// `#[foo]` is an error naming the syntax the item needs — the right answer wherever
    /// the attribute exists to carry values, as `#[param(..)]` and `#[plugin(..)]` do.
    Rejected,
    /// `#[foo]` means "this item, with every default", as `#[state]` does on a field.
    Allowed,
}

impl AttrSet {
    /// Collects and validates every `#[<path>(..)]` attribute on one item, rejecting a
    /// bare `#[<path>]`.
    ///
    /// `valid` is the exhaustive list of accepted keys; anything else is a compile error
    /// that lists them (and suggests the nearest one when the key looks like a typo).
    /// `fallback` is the span blamed when the attribute is absent entirely.
    pub(crate) fn parse(
        attrs: &[Attribute],
        path: &str,
        context: &'static str,
        valid: &'static [&'static str],
        fallback: Span,
    ) -> Result<Self> {
        Self::parse_with(attrs, path, context, valid, fallback, Bare::Rejected)
    }

    /// As [`AttrSet::parse`], but a bare `#[<path>]` is accepted and produces a present,
    /// empty set.
    pub(crate) fn parse_allowing_bare(
        attrs: &[Attribute],
        path: &str,
        context: &'static str,
        valid: &'static [&'static str],
        fallback: Span,
    ) -> Result<Self> {
        Self::parse_with(attrs, path, context, valid, fallback, Bare::Allowed)
    }

    /// The shared body of the two constructors.
    fn parse_with(
        attrs: &[Attribute],
        path: &str,
        context: &'static str,
        valid: &'static [&'static str],
        fallback: Span,
        bare: Bare,
    ) -> Result<Self> {
        let mut set = Self {
            context,
            span: fallback,
            present: false,
            entries: Vec::new(),
        };

        for attr in attrs.iter().filter(|a| a.path().is_ident(path)) {
            if !set.present {
                set.span = attr.span();
            }
            set.present = true;

            if matches!(attr.meta, syn::Meta::Path(_)) {
                if bare == Bare::Allowed {
                    continue;
                }
                return Err(Error::new_spanned(
                    attr,
                    format!(
                        "`#[{path}]` needs arguments\n  write `{context}`\n  valid keys: {}",
                        valid.join(", ")
                    ),
                ));
            }

            let parsed =
                attr.parse_args_with(Punctuated::<AttrEntry, Token![,]>::parse_terminated)?;
            for entry in parsed {
                let name = entry.name();
                if !valid.contains(&name.as_str()) {
                    return Err(unknown_key(context, &entry.key, valid));
                }
                if let Some(previous) = set.entries.iter().find(|e| e.name() == name) {
                    let mut error = Error::new(
                        entry.span(),
                        format!("`{name}` is set twice in `{context}`"),
                    );
                    error.combine(Error::new(
                        previous.span(),
                        format!("`{name}` is first set here"),
                    ));
                    return Err(error);
                }
                set.entries.push(entry);
            }
        }

        Ok(set)
    }

    /// Looks one key up.
    pub(crate) fn get(&self, key: &str) -> Option<&AttrEntry> {
        self.entries.iter().find(|entry| entry.name() == key)
    }

    /// `true` when the key is present, whatever its value.
    pub(crate) fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// The first of `keys` that is present, if any.
    pub(crate) fn first_of(&self, keys: &[&str]) -> Option<&AttrEntry> {
        self.entries
            .iter()
            .find(|entry| keys.contains(&entry.name().as_str()))
    }

    /// `true` when no entry was written.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Reads the `crate = <path>` override every derive accepts, defaulting to
    /// `::daux_plugin`.
    pub(crate) fn crate_path(&self) -> Result<Path> {
        match self.get("crate") {
            Some(entry) => entry.path_value(self.context),
            None => Ok(syn::parse_quote!(::daux_plugin)),
        }
    }

    /// Rejects a key that is present but meaningless for this item.
    ///
    /// `because` completes the sentence "… does not apply here because …".
    pub(crate) fn reject(&self, key: &str, because: &str) -> Result<()> {
        match self.get(key) {
            Some(entry) => Err(Error::new(
                entry.span(),
                format!("`{key}` does not apply here: {because}"),
            )),
            None => Ok(()),
        }
    }

    /// Builds an error blaming the whole attribute, used for missing required keys.
    pub(crate) fn error(&self, message: impl Into<String>) -> Error {
        Error::new(self.span, message.into())
    }
}

/// Builds the "unknown key" error, complete with the list of valid keys and, when the
/// key looks like a near miss, a suggestion.
pub(crate) fn unknown_key(context: &str, key: &Ident, valid: &[&str]) -> Error {
    let name = key.unraw().to_string();
    let mut message = format!(
        "unknown `{context}` key `{name}`\n  valid keys: {}",
        valid.join(", ")
    );
    if let Some(closest) = suggest(&name, valid) {
        message.push_str(&format!("\n  did you mean `{closest}`?"));
    }
    Error::new(key.span(), message)
}

/// Picks the closest valid key to `input`, when one is close enough to be a typo.
///
/// The threshold scales with the length of the word so that `id` does not "suggest"
/// every two-letter key while `capabilites` still finds `capabilities`.
pub(crate) fn suggest<'a>(input: &str, valid: &[&'a str]) -> Option<&'a str> {
    let budget = match input.chars().count() {
        0..=3 => 1,
        4..=8 => 2,
        _ => 3,
    };
    valid
        .iter()
        .map(|candidate| (levenshtein(input, candidate), *candidate))
        .filter(|(distance, _)| *distance <= budget)
        .min_by_key(|(distance, candidate)| (*distance, candidate.len()))
        .map(|(_, candidate)| candidate)
}

/// Plain Levenshtein distance over `char`s, two rows at a time.
fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    let mut current: Vec<usize> = vec![0; b_chars.len() + 1];

    for (i, a_char) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, b_char) in b_chars.iter().enumerate() {
            let substitution = previous[j] + usize::from(a_char != *b_char);
            let deletion = previous[j + 1] + 1;
            let insertion = current[j] + 1;
            current[j + 1] = substitution.min(deletion).min(insertion);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[b_chars.len()]
}

/// Splits an integer literal expression into its sign and its literal, seeing through a
/// leading `-`.
pub(crate) fn int_literal(expr: &Expr) -> Option<(bool, &LitInt)> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(lit), ..
        }) => Some((false, lit)),
        Expr::Unary(ExprUnary {
            op: UnOp::Neg(_),
            expr,
            ..
        }) => match expr.as_ref() {
            Expr::Lit(ExprLit {
                lit: Lit::Int(lit), ..
            }) => Some((true, lit)),
            _ => None,
        },
        _ => None,
    }
}

/// The value of a numeric literal expression, seeing through a leading `-`.
///
/// Returns `None` for anything that is not a literal — a constant, a `const fn` call or
/// an arithmetic expression — so that literal-only validation simply steps aside rather
/// than guessing.
pub(crate) fn numeric_literal(expr: &Expr) -> Option<f64> {
    fn value(lit: &Lit) -> Option<f64> {
        match lit {
            Lit::Float(lit) => lit.base10_parse::<f64>().ok(),
            Lit::Int(lit) => lit.base10_parse::<i64>().ok().map(|v| v as f64),
            _ => None,
        }
    }
    match expr {
        Expr::Lit(ExprLit { lit, .. }) => value(lit),
        Expr::Unary(ExprUnary {
            op: UnOp::Neg(_),
            expr,
            ..
        }) => match expr.as_ref() {
            Expr::Lit(ExprLit { lit, .. }) => value(lit).map(|v| -v),
            _ => None,
        },
        _ => None,
    }
}

/// Re-emits `expr` as an `f64`-typed expression.
///
/// `range = 0..=1` on a float parameter is a natural thing to write and would otherwise
/// fail with "expected `f64`, found integer"; rewriting the literal keeps the friendly
/// spelling working without inserting an `as` cast that would also silently accept a
/// non-numeric expression.
pub(crate) fn as_f64_expr(expr: &Expr) -> proc_macro2::TokenStream {
    match int_literal(expr) {
        Some((negative, lit)) => {
            let float = LitFloat::new(&format!("{}f64", lit.base10_digits()), lit.span());
            if negative {
                quote::quote!(-#float)
            } else {
                float.into_token_stream()
            }
        }
        None => expr.into_token_stream(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(source: &str) -> Vec<Attribute> {
        let item: syn::DeriveInput = syn::parse_str(source).expect("test input parses");
        item.attrs
    }

    fn parse(source: &str, valid: &'static [&'static str]) -> Result<AttrSet> {
        let attrs = attrs(source);
        AttrSet::parse(&attrs, "param", "#[param(..)]", valid, Span::call_site())
    }

    const KEYS: &[&str] = &["id", "name", "range", "flags", "skip", "crate"];

    #[test]
    fn parses_flags_values_and_lists() {
        let set = parse(
            "#[param(skip, id = 1, name = \"Gain\", flags(automatable, hidden))] struct S;",
            KEYS,
        )
        .expect("valid attribute");

        assert!(set.present);
        assert!(!set.is_empty());
        assert!(set.has("skip"));
        assert!(matches!(
            set.get("skip").expect("skip").value,
            AttrValue::Flag
        ));
        assert_eq!(
            set.get("id")
                .expect("id")
                .int_value("#[param(..)]")
                .expect("literal")
                .base10_parse::<u32>()
                .expect("u32"),
            1
        );
        assert_eq!(
            set.get("name")
                .expect("name")
                .str_value("#[param(..)]")
                .expect("literal")
                .value(),
            "Gain"
        );
        let flags = set
            .get("flags")
            .expect("flags")
            .ident_list("#[param(..)]")
            .expect("names");
        assert_eq!(
            flags.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["automatable", "hidden"]
        );
    }

    #[test]
    fn an_absent_attribute_is_reported_as_absent() {
        let set = parse("struct S;", KEYS).expect("no attribute is fine");
        assert!(!set.present);
        assert!(set.is_empty());
        assert!(set.get("id").is_none());
    }

    #[test]
    fn an_empty_list_is_still_a_list() {
        let set = parse("#[param(flags())] struct S;", KEYS).expect("valid");
        assert!(
            set.get("flags")
                .expect("flags")
                .ident_list("#[param(..)]")
                .expect("names")
                .is_empty()
        );
    }

    #[test]
    fn several_attributes_merge() {
        let set = parse("#[param(id = 1)] #[param(name = \"Gain\")] struct S;", KEYS)
            .expect("merging is allowed");
        assert!(set.has("id") && set.has("name"));
    }

    #[test]
    fn an_unknown_key_lists_the_valid_ones_and_suggests_the_nearest() {
        let error = parse("#[param(nmae = \"Gain\")] struct S;", KEYS).expect_err("unknown key");
        let message = error.to_string();
        assert!(
            message.contains("unknown `#[param(..)]` key `nmae`"),
            "{message}"
        );
        assert!(
            message.contains("valid keys: id, name, range, flags, skip, crate"),
            "{message}"
        );
        assert!(message.contains("did you mean `name`?"), "{message}");
    }

    #[test]
    fn an_unrelated_key_gets_no_suggestion() {
        let error = parse("#[param(oscillator_shape = 1)] struct S;", KEYS).expect_err("unknown");
        assert!(!error.to_string().contains("did you mean"), "{error}");
    }

    #[test]
    fn a_repeated_key_names_both_occurrences() {
        let error = parse("#[param(id = 1, id = 2)] struct S;", KEYS).expect_err("duplicate key");
        let messages: Vec<String> = error.into_iter().map(|e| e.to_string()).collect();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(messages[0].contains("`id` is set twice"), "{messages:?}");
        assert!(
            messages[1].contains("`id` is first set here"),
            "{messages:?}"
        );
    }

    #[test]
    fn a_bare_attribute_is_present_but_empty_when_it_is_allowed() {
        let bare = attrs("#[state] struct S;");
        let set =
            AttrSet::parse_allowing_bare(&bare, "state", "#[state(..)]", KEYS, Span::call_site())
                .expect("a bare attribute is the whole point here");
        assert!(set.present, "the attribute was written");
        assert!(set.is_empty(), "it carries no entries");
        assert!(set.get("id").is_none());

        // A bare attribute must not erase the entries of a second, non-bare one.
        let merged = attrs("#[state] #[state(id = 1)] struct S;");
        let set =
            AttrSet::parse_allowing_bare(&merged, "state", "#[state(..)]", KEYS, Span::call_site())
                .expect("merging is allowed");
        assert!(set.has("id"), "the second attribute still counts");
    }

    #[test]
    fn is_flag_distinguishes_the_three_shapes() {
        let set = parse("#[param(skip, id = 1, flags(hidden))] struct S;", KEYS).expect("parses");
        assert!(set.get("skip").expect("skip").is_flag());
        assert!(!set.get("id").expect("id").is_flag());
        assert!(!set.get("flags").expect("flags").is_flag());
    }

    #[test]
    fn a_bare_attribute_explains_the_syntax() {
        let attrs = attrs("#[param] struct S;");
        let error = AttrSet::parse(&attrs, "param", "#[param(..)]", KEYS, Span::call_site())
            .expect_err("bare attribute");
        assert!(error.to_string().contains("needs arguments"), "{error}");
    }

    #[test]
    fn value_accessors_reject_the_wrong_shape() {
        let set = parse("#[param(skip, name = 1, flags = 2)] struct S;", KEYS).expect("parses");

        let error = set
            .get("skip")
            .expect("skip")
            .expr("#[param(..)]")
            .expect_err("flag has no value");
        assert!(
            error.to_string().contains("`skip` needs a value"),
            "{error}"
        );

        let error = set
            .get("name")
            .expect("name")
            .str_value("#[param(..)]")
            .expect_err("not a string");
        assert!(
            error.to_string().contains("must be a string literal"),
            "{error}"
        );

        let error = set
            .get("flags")
            .expect("flags")
            .list("#[param(..)]")
            .expect_err("not a list");
        assert!(
            error.to_string().contains("needs a parenthesised list"),
            "{error}"
        );

        let error = set
            .get("skip")
            .expect("skip")
            .int_value("#[param(..)]")
            .expect_err("flag has no value");
        assert!(error.to_string().contains("needs a value"), "{error}");
    }

    #[test]
    fn a_list_element_that_is_not_a_name_is_rejected() {
        let set = parse("#[param(flags(1 + 2))] struct S;", KEYS).expect("parses");
        let error = set
            .get("flags")
            .expect("flags")
            .ident_list("#[param(..)]")
            .expect_err("not a name");
        assert!(error.to_string().contains("takes bare names"), "{error}");
    }

    #[test]
    fn expect_flag_rejects_a_value() {
        let set = parse("#[param(skip = 1)] struct S;", KEYS).expect("parses");
        let error = set
            .get("skip")
            .expect("skip")
            .expect_flag("#[param(..)]")
            .expect_err("flag with a value");
        assert!(error.to_string().contains("takes no value"), "{error}");
    }

    #[test]
    fn the_crate_keyword_is_accepted_as_a_key() {
        let set = parse("#[param(crate = ::my_sdk)] struct S;", KEYS).expect("crate is a key");
        let path = set.crate_path().expect("path");
        assert_eq!(quote::quote!(#path).to_string(), ":: my_sdk");
    }

    #[test]
    fn the_crate_path_defaults_to_the_facade() {
        let set = parse("struct S;", KEYS).expect("no attribute");
        let path = set.crate_path().expect("default path");
        assert_eq!(quote::quote!(#path).to_string(), ":: daux_plugin");
    }

    #[test]
    fn a_crate_override_that_is_not_a_path_is_rejected() {
        let set = parse("#[param(crate = \"daux\")] struct S;", KEYS).expect("parses");
        let error = set.crate_path().expect_err("string is not a path");
        assert!(error.to_string().contains("must be a path"), "{error}");
    }

    #[test]
    fn reject_explains_why_a_key_does_not_belong() {
        let set = parse("#[param(range = 0..=1)] struct S;", KEYS).expect("parses");
        let error = set
            .reject("range", "a switch is always off or on")
            .expect_err("rejected");
        assert!(
            error
                .to_string()
                .contains("`range` does not apply here: a switch is always off or on"),
            "{error}"
        );
        assert!(set.reject("name", "unused").is_ok());
    }

    #[test]
    fn first_of_returns_the_earliest_written_key() {
        let set = parse("#[param(name = \"Gain\", range = 0..=1)] struct S;", KEYS).expect("ok");
        assert_eq!(
            set.first_of(&["range", "name"])
                .expect("one of them")
                .name(),
            "name"
        );
        assert!(set.first_of(&["skip"]).is_none());
    }

    #[test]
    fn levenshtein_matches_known_distances() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("flaw", "lawn"), 2);
        assert_eq!(levenshtein("näme", "name"), 1);
    }

    #[test]
    fn suggestions_scale_with_word_length() {
        assert_eq!(suggest("nmae", KEYS), Some("name"));
        assert_eq!(
            suggest("capabilites", &["capabilities", "category"]),
            Some("capabilities")
        );
        assert_eq!(suggest("xyzzy", KEYS), None);
        // A short key must not attract a suggestion from every other short key.
        assert_eq!(suggest("qq", &["id", "name"]), None);
    }

    #[test]
    fn numeric_literals_are_read_through_a_leading_minus() {
        let expr: Expr = syn::parse_str("-60.0").expect("expr");
        assert_eq!(numeric_literal(&expr), Some(-60.0));
        let expr: Expr = syn::parse_str("12").expect("expr");
        assert_eq!(numeric_literal(&expr), Some(12.0));
        let expr: Expr = syn::parse_str("MAX_GAIN").expect("expr");
        assert_eq!(numeric_literal(&expr), None);
        let expr: Expr = syn::parse_str("-MAX_GAIN").expect("expr");
        assert_eq!(numeric_literal(&expr), None);
        let expr: Expr = syn::parse_str("\"12\"").expect("expr");
        assert_eq!(numeric_literal(&expr), None);
    }

    #[test]
    fn integer_literals_become_float_literals() {
        let expr: Expr = syn::parse_str("0").expect("expr");
        assert_eq!(as_f64_expr(&expr).to_string(), "0f64");
        let expr: Expr = syn::parse_str("-60").expect("expr");
        assert_eq!(as_f64_expr(&expr).to_string(), "- 60f64");
        // Anything already floating point, or not a literal at all, is left alone.
        let expr: Expr = syn::parse_str("-60.0").expect("expr");
        assert_eq!(as_f64_expr(&expr).to_string(), "- 60.0");
        let expr: Expr = syn::parse_str("MAX_GAIN").expect("expr");
        assert_eq!(as_f64_expr(&expr).to_string(), "MAX_GAIN");
    }
}

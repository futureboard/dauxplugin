//! `#[derive(DauxParams)]` — the parameter bank.

use proc_macro2::{Span, TokenStream};
use quote::{ToTokens, quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{
    Data, DeriveInput, Error, Expr, ExprLit, ExprRange, Fields, Ident, Lit, LitFloat, LitInt,
    LitStr, Path, RangeLimits, Result, Type, Visibility,
};

use crate::attr::{self, AttrSet};

/// How the attribute is spelled, for diagnostics.
const FIELD_CONTEXT: &str = "#[param(..)]";
/// How the container attribute is spelled, for diagnostics.
const CONTAINER_CONTEXT: &str = "#[params(..)]";

/// Every key `#[param(..)]` accepts.
const FIELD_KEYS: &[&str] = &[
    "id",
    "name",
    "range",
    "default",
    "unit",
    "curve",
    "flags",
    "group",
    "smoothing",
    "decimals",
    "labels",
    "skip",
];

/// Every key `#[params(..)]` accepts.
const CONTAINER_KEYS: &[&str] = &["state_schema_version", "migrations", "crate"];

/// The keys that describe *how to build* a parameter, as opposed to identifying one.
///
/// Writing any of them is a promise that the field can be constructed from its
/// attribute alone, which is what makes the generated `new()` possible.
const CONSTRUCTION_KEYS: &[&str] = &[
    "name",
    "range",
    "default",
    "unit",
    "curve",
    "flags",
    "group",
    "smoothing",
    "decimals",
    "labels",
];

/// Every flag name accepted inside `flags(..)`, and the `ParamFlags` constant it maps to.
const FLAGS: &[(&str, &str)] = &[
    ("automatable", "AUTOMATABLE"),
    ("modulatable", "MODULATABLE"),
    ("per_note", "PER_NOTE"),
    ("stepped", "STEPPED"),
    ("read_only", "READ_ONLY"),
    ("hidden", "HIDDEN"),
    ("bypass", "BYPASS"),
    ("requires_process", "REQUIRES_PROCESS"),
    ("is_meter", "IS_METER"),
];

/// The concrete parameter types the derive knows how to construct.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ParamKind {
    /// [`daux_parameter::FloatParam`](../../daux_parameter/struct.FloatParam.html).
    Float,
    /// An integer parameter.
    Int,
    /// A switch.
    Bool,
    /// An enumerated selector.
    Enum,
    /// A read-only meter.
    Meter,
}

impl ParamKind {
    /// Recognises the parameter type from the field's *written* type.
    ///
    /// Only the last path segment is inspected, so `daux_parameter::FloatParam`,
    /// `param::FloatParam` and `FloatParam` are all understood. A type alias is not:
    /// a macro sees names, never types.
    pub(crate) fn from_type(ty: &Type) -> Option<Self> {
        let Type::Path(path) = ty else {
            return None;
        };
        if path.qself.is_some() {
            return None;
        }
        match path.path.segments.last()?.ident.to_string().as_str() {
            "FloatParam" => Some(Self::Float),
            "IntParam" => Some(Self::Int),
            "BoolParam" => Some(Self::Bool),
            "EnumParam" => Some(Self::Enum),
            "MeterParam" => Some(Self::Meter),
            _ => None,
        }
    }

    /// The name used in diagnostics.
    fn name(self) -> &'static str {
        match self {
            Self::Float => "FloatParam",
            Self::Int => "IntParam",
            Self::Bool => "BoolParam",
            Self::Enum => "EnumParam",
            Self::Meter => "MeterParam",
        }
    }
}

/// How one field's id was written.
pub(crate) enum ParamIdSpec {
    /// `id = 1`.
    Literal {
        /// The parsed value, used for compile-time duplicate detection.
        value: u32,
        /// Where it was written.
        span: Span,
    },
    /// `id = "gain"`, hashed by `ParamId::from_name`.
    Name(LitStr),
    /// `id = MY_GAIN_ID`, any other constant expression of type `u32`.
    Expr(Expr),
}

impl ParamIdSpec {
    /// Where the id was written.
    fn span(&self) -> Span {
        match self {
            Self::Literal { span, .. } => *span,
            Self::Name(lit) => lit.span(),
            Self::Expr(expr) => expr.span(),
        }
    }

    /// `true` when the value is known while the macro runs.
    fn is_literal(&self) -> bool {
        matches!(self, Self::Literal { .. })
    }
}

/// One parameter field.
pub(crate) struct ParamField {
    /// The field's name, also the identifier used in generated code.
    pub(crate) ident: Ident,
    /// The field's id.
    pub(crate) id: ParamIdSpec,
    /// How to build it, when the attribute says enough to build it at all.
    pub(crate) build: Option<Build>,
}

/// Everything needed to construct one parameter.
pub(crate) struct Build {
    /// Which concrete type to construct.
    kind: ParamKind,
    /// Display name.
    name: LitStr,
    /// The range expression, already turned into a `ParamRange` or a `min`/`max` pair.
    range: Option<RangeSpec>,
    /// Default value expression, in the parameter's own units.
    default: Option<Expr>,
    /// Unit suffix.
    unit: Option<LitStr>,
    /// Group path.
    group: Option<LitStr>,
    /// Explicit flags.
    flags: Option<Vec<Ident>>,
    /// Fraction digits.
    decimals: Option<LitInt>,
    /// Smoothing intent.
    smoothing: Option<SmoothingSpec>,
    /// `BoolParam` state labels.
    labels: Option<(LitStr, LitStr)>,
}

/// A parsed `range = a..=b` plus the curve that maps it.
struct RangeSpec {
    /// Lower bound as written.
    min: Expr,
    /// Upper bound as written.
    max: Expr,
    /// The curve, defaulting to linear.
    curve: Curve,
}

/// The value curve requested by `curve = ".."`.
enum Curve {
    /// `curve = "linear"`, the default.
    Linear,
    /// `curve = "log"`.
    Logarithmic,
    /// `curve = "skew(2.0)"`.
    Skewed(LitFloat),
    /// `curve = "stepped"`.
    Stepped,
}

/// The parsed `smoothing = ".."` value.
enum SmoothingSpec {
    /// `smoothing = "none"`.
    None,
    /// `smoothing = "linear(20.0)"`, in milliseconds.
    Linear(LitFloat),
    /// `smoothing = "exponential(20.0)"`, in milliseconds.
    Exponential(LitFloat),
}

/// The whole derive input, after parsing and validation.
pub(crate) struct ParamsInput {
    ident: Ident,
    vis: Visibility,
    generics: syn::Generics,
    krate: Path,
    state_schema_version: Option<Expr>,
    migrations: Option<Path>,
    fields: Vec<ParamField>,
    /// `true` when every field of the struct is a parameter, which is the only case in
    /// which a generated `new()` could name them all.
    every_field_is_a_param: bool,
}

/// Entry point: parse, validate, then generate.
pub(crate) fn derive(input: &DeriveInput) -> Result<TokenStream> {
    Ok(expand(&parse(input)?))
}

// ------------------------------------------------------------------------ parsing ---

/// Parses and fully validates a `#[derive(DauxParams)]` input.
pub(crate) fn parse(input: &DeriveInput) -> Result<ParamsInput> {
    let container = AttrSet::parse(
        &input.attrs,
        "params",
        CONTAINER_CONTEXT,
        CONTAINER_KEYS,
        input.ident.span(),
    )?;

    let Data::Struct(data) = &input.data else {
        return Err(Error::new_spanned(
            &input.ident,
            "`#[derive(DauxParams)]` describes a parameter bank, so it needs a struct with \
             named fields\n  write `struct MyParams { #[param(id = 1, ..)] gain: FloatParam }`",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(Error::new_spanned(
            &input.ident,
            "`#[derive(DauxParams)]` needs named fields: every parameter is reached by \
             field name in the generated `Params` impl",
        ));
    };

    let mut fields = Vec::new();
    let mut every_field_is_a_param = true;
    for field in &named.named {
        let ident = field
            .ident
            .clone()
            .unwrap_or_else(|| unreachable!("named fields always have an identifier"));
        let attrs = AttrSet::parse(
            &field.attrs,
            "param",
            FIELD_CONTEXT,
            FIELD_KEYS,
            ident.span(),
        )?;

        if !attrs.present || attrs.has("skip") {
            every_field_is_a_param = false;
            if let Some(entry) = attrs.get("skip") {
                entry.expect_flag(FIELD_CONTEXT)?;
                if let Some(other) = attrs
                    .first_of(&["id"])
                    .or_else(|| attrs.first_of(CONSTRUCTION_KEYS))
                {
                    return Err(Error::new(
                        other.span(),
                        format!(
                            "`{}` cannot be combined with `skip`: a skipped field is not a \
                             parameter\n  remove `skip`, or remove the other keys",
                            other.name()
                        ),
                    ));
                }
            }
            continue;
        }

        fields.push(parse_field(&ident, &field.ty, &attrs)?);
    }

    check_unique_ids(&input.ident, &fields)?;

    Ok(ParamsInput {
        ident: input.ident.clone(),
        vis: input.vis.clone(),
        generics: input.generics.clone(),
        krate: container.crate_path()?,
        state_schema_version: match container.get("state_schema_version") {
            Some(entry) => Some(entry.expr(CONTAINER_CONTEXT)?.clone()),
            None => None,
        },
        migrations: match container.get("migrations") {
            Some(entry) => Some(entry.path_value(CONTAINER_CONTEXT)?),
            None => None,
        },
        fields,
        every_field_is_a_param,
    })
}

/// Parses one annotated field.
fn parse_field(ident: &Ident, ty: &Type, attrs: &AttrSet) -> Result<ParamField> {
    let id = parse_id(ident, attrs)?;
    let kind = ParamKind::from_type(ty);
    let wants_construction = attrs.first_of(CONSTRUCTION_KEYS).is_some();

    let build = if wants_construction {
        let Some(kind) = kind else {
            let culprit = attrs
                .first_of(CONSTRUCTION_KEYS)
                .unwrap_or_else(|| unreachable!("just checked that one is present"));
            return Err(Error::new(
                culprit.span(),
                format!(
                    "`#[derive(DauxParams)]` cannot build `{}` from attributes\n  it knows \
                     FloatParam, IntParam, BoolParam, EnumParam and MeterParam\n  keep only \
                     `id = ..` on field `{ident}` and build it in your own `new()`",
                    ty.to_token_stream(),
                ),
            ));
        };
        Some(parse_build(ident, kind, attrs)?)
    } else {
        None
    };

    Ok(ParamField {
        ident: ident.clone(),
        id,
        build,
    })
}

/// Parses `id = ..`, the one key every parameter must have.
fn parse_id(ident: &Ident, attrs: &AttrSet) -> Result<ParamIdSpec> {
    let Some(entry) = attrs.get("id") else {
        return Err(attrs.error(format!(
            "field `{ident}` has no parameter id\n  write `#[param(id = 1, name = \"…\")]`, \
             `#[param(id = \"{ident}\")]` to hash the name, or `#[param(skip)]` if this field \
             is not a parameter\n  ids are permanent: renaming a parameter is free, \
             renumbering corrupts saved projects"
        )));
    };

    let expr = entry.expr(FIELD_CONTEXT)?;
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(lit), ..
        }) => Ok(ParamIdSpec::Literal {
            value: lit.base10_parse::<u32>()?,
            span: lit.span(),
        }),
        Expr::Lit(ExprLit {
            lit: Lit::Str(lit), ..
        }) => Ok(ParamIdSpec::Name(lit.clone())),
        Expr::Lit(ExprLit { lit, .. }) => Err(Error::new_spanned(
            lit,
            "`id` must be a `u32` literal, a name to hash, or a constant expression\n  \
             write `id = 1`, `id = \"gain\"` or `id = MY_GAIN_ID`",
        )),
        other => Ok(ParamIdSpec::Expr(other.clone())),
    }
}

/// Parses everything needed to construct the parameter.
fn parse_build(ident: &Ident, kind: ParamKind, attrs: &AttrSet) -> Result<Build> {
    reject_inapplicable(kind, attrs)?;

    let Some(name_entry) = attrs.get("name") else {
        let culprit = attrs
            .first_of(CONSTRUCTION_KEYS)
            .unwrap_or_else(|| unreachable!("construction was requested"));
        return Err(Error::new(
            culprit.span(),
            format!(
                "field `{ident}` is built from its attribute but has no `name`\n  add \
                 `name = \"…\"`, or keep only `id = ..` and build it in your own `new()`"
            ),
        ));
    };
    let name = name_entry.str_value(FIELD_CONTEXT)?;

    let range = match attrs.get("range") {
        Some(entry) => Some(parse_range(kind, entry.expr(FIELD_CONTEXT)?, attrs)?),
        None => {
            if matches!(kind, ParamKind::Float | ParamKind::Int | ParamKind::Meter) {
                return Err(Error::new(
                    name_entry.span(),
                    format!(
                        "field `{ident}` is a {} and needs a range\n  write \
                         `range = -60.0..=12.0` (inclusive, in the parameter's own units)",
                        kind.name()
                    ),
                ));
            }
            None
        }
    };

    let default = match attrs.get("default") {
        Some(entry) => Some(entry.expr(FIELD_CONTEXT)?.clone()),
        None => {
            if matches!(kind, ParamKind::Float | ParamKind::Int | ParamKind::Enum) {
                return Err(Error::new(
                    name_entry.span(),
                    format!(
                        "field `{ident}` is a {} and needs a default value\n  write \
                         `default = 0.0` — the value the parameter resets to",
                        kind.name()
                    ),
                ));
            }
            None
        }
    };

    if let (Some(range), Some(default)) = (range.as_ref(), default.as_ref()) {
        check_default_in_range(range, default)?;
    }

    let flags = match attrs.get("flags") {
        Some(entry) => {
            let names = entry.ident_list(FIELD_CONTEXT)?;
            for flag in &names {
                let written = flag.to_string();
                if !FLAGS.iter().any(|(name, _)| *name == written) {
                    let valid: Vec<&str> = FLAGS.iter().map(|(name, _)| *name).collect();
                    let mut message = format!(
                        "unknown parameter flag `{written}`\n  valid flags: {}",
                        valid.join(", ")
                    );
                    if let Some(closest) = attr::suggest(&written, &valid) {
                        message.push_str(&format!("\n  did you mean `{closest}`?"));
                    }
                    return Err(Error::new(flag.span(), message));
                }
            }
            Some(names)
        }
        None => None,
    };

    let decimals = match attrs.get("decimals") {
        Some(entry) => {
            let lit = entry.int_value(FIELD_CONTEXT)?;
            lit.base10_parse::<u8>()?;
            Some(lit)
        }
        None => None,
    };

    let smoothing = match attrs.get("smoothing") {
        Some(entry) => Some(parse_smoothing(&entry.str_value(FIELD_CONTEXT)?)?),
        None => None,
    };

    let labels = match attrs.get("labels") {
        Some(entry) => {
            let items = entry.list(FIELD_CONTEXT)?;
            let [off, on] = items else {
                return Err(Error::new(
                    entry.span(),
                    "`labels` takes exactly two string literals\n  write \
                     `labels(\"Normal\", \"Inverted\")`",
                ));
            };
            Some((
                string_literal(off, "labels")?,
                string_literal(on, "labels")?,
            ))
        }
        None => None,
    };

    Ok(Build {
        kind,
        name,
        range,
        default,
        unit: match attrs.get("unit") {
            Some(entry) => Some(entry.str_value(FIELD_CONTEXT)?),
            None => None,
        },
        group: match attrs.get("group") {
            Some(entry) => Some(entry.str_value(FIELD_CONTEXT)?),
            None => None,
        },
        flags,
        decimals,
        smoothing,
        labels,
    })
}

/// Rejects keys that make no sense for this parameter type, with the reason.
fn reject_inapplicable(kind: ParamKind, attrs: &AttrSet) -> Result<()> {
    match kind {
        ParamKind::Float => attrs.reject("labels", "`labels` belongs to a BoolParam"),
        ParamKind::Int => {
            attrs.reject("curve", "an IntParam is always a stepped, linear range")?;
            attrs.reject(
                "smoothing",
                "an IntParam is not ramped; smooth its effect instead",
            )?;
            attrs.reject("decimals", "an IntParam prints whole numbers")?;
            attrs.reject("labels", "`labels` belongs to a BoolParam")
        }
        ParamKind::Bool => {
            attrs.reject("range", "a BoolParam is off or on")?;
            attrs.reject("curve", "a BoolParam is off or on")?;
            attrs.reject("unit", "a BoolParam prints its labels; use `labels(..)`")?;
            attrs.reject("decimals", "a BoolParam prints its labels")?;
            attrs.reject("smoothing", "a BoolParam is not ramped")
        }
        ParamKind::Enum => {
            attrs.reject("range", "an EnumParam covers exactly its variants")?;
            attrs.reject("curve", "an EnumParam covers exactly its variants")?;
            attrs.reject("unit", "an EnumParam prints variant names")?;
            attrs.reject("decimals", "an EnumParam prints variant names")?;
            attrs.reject("smoothing", "an EnumParam is not ramped")?;
            attrs.reject("labels", "`labels` belongs to a BoolParam")
        }
        ParamKind::Meter => {
            attrs.reject(
                "default",
                "a MeterParam is written by the processor, never reset",
            )?;
            attrs.reject("smoothing", "a MeterParam is not automated")?;
            attrs.reject("labels", "`labels` belongs to a BoolParam")
        }
    }
}

/// Parses `range = a..=b` and folds the `curve` key into it.
fn parse_range(kind: ParamKind, expr: &Expr, attrs: &AttrSet) -> Result<RangeSpec> {
    let Expr::Range(ExprRange {
        start: Some(start),
        limits: RangeLimits::Closed(_),
        end: Some(end),
        ..
    }) = expr
    else {
        return Err(Error::new_spanned(
            expr,
            "`range` must be an inclusive range with both bounds\n  write \
             `range = -60.0..=12.0` (`..=`, not `..`)",
        ));
    };

    let curve = match attrs.get("curve") {
        Some(entry) => parse_curve(&entry.str_value(FIELD_CONTEXT)?)?,
        None if kind == ParamKind::Int => Curve::Stepped,
        None => Curve::Linear,
    };

    let range = RangeSpec {
        min: start.as_ref().clone(),
        max: end.as_ref().clone(),
        curve,
    };
    check_range(kind, &range)?;
    Ok(range)
}

/// Compile-time validation of the bounds, so that a range the runtime would reject is a
/// build error rather than a panic during the author's first run.
fn check_range(kind: ParamKind, range: &RangeSpec) -> Result<()> {
    if kind == ParamKind::Int || matches!(range.curve, Curve::Stepped) {
        for bound in [&range.min, &range.max] {
            if matches!(
                bound,
                Expr::Lit(ExprLit {
                    lit: Lit::Float(_),
                    ..
                })
            ) {
                return Err(Error::new_spanned(
                    bound,
                    "a stepped range counts in whole numbers\n  write `range = 1..=16`",
                ));
            }
        }
    }

    let (Some(min), Some(max)) = (
        attr::numeric_literal(&range.min),
        attr::numeric_literal(&range.max),
    ) else {
        // At least one bound is a constant or an expression: leave it to the runtime,
        // which validates every range it is given.
        return Ok(());
    };

    if !min.is_finite() || !max.is_finite() {
        return Err(Error::new_spanned(
            &range.min,
            "range bounds must be finite",
        ));
    }
    if min == max {
        return Err(Error::new_spanned(
            &range.max,
            format!(
                "range bounds must differ, but both are {min}\n  a range of one value \
                     cannot be mapped to a knob"
            ),
        ));
    }
    if min > max {
        return Err(Error::new_spanned(
            &range.max,
            format!("range bounds are inverted: {min}..={max}\n  write the lower bound first"),
        ));
    }
    if matches!(range.curve, Curve::Logarithmic) && min <= 0.0 {
        return Err(Error::new_spanned(
            &range.min,
            format!(
                "a logarithmic range needs strictly positive bounds, but the lower bound is \
                 {min}\n  zero has no logarithm — write something like `range = 20.0..=20000.0`"
            ),
        ));
    }
    Ok(())
}

/// Rejects a default that the runtime would silently clamp.
fn check_default_in_range(range: &RangeSpec, default: &Expr) -> Result<()> {
    let (Some(min), Some(max), Some(value)) = (
        attr::numeric_literal(&range.min),
        attr::numeric_literal(&range.max),
        attr::numeric_literal(default),
    ) else {
        return Ok(());
    };
    if value < min || value > max {
        return Err(Error::new_spanned(
            default,
            format!(
                "default {value} is outside the range {min}..={max}\n  it would be silently \
                 clamped, which is never what the author meant"
            ),
        ));
    }
    Ok(())
}

/// Parses the `curve = ".."` spelling.
fn parse_curve(lit: &LitStr) -> Result<Curve> {
    let text = lit.value();
    let trimmed = text.trim();
    match trimmed {
        "linear" => return Ok(Curve::Linear),
        "log" | "logarithmic" => return Ok(Curve::Logarithmic),
        "stepped" => return Ok(Curve::Stepped),
        _ => {}
    }
    if let Some(factor) = trimmed
        .strip_prefix("skew(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let factor = factor.trim();
        let parsed = factor.parse::<f64>().map_err(|_| {
            Error::new(
                lit.span(),
                format!("`skew({factor})` needs a number\n  write `curve = \"skew(2.0)\"`"),
            )
        })?;
        if !parsed.is_finite() || parsed <= 0.0 {
            return Err(Error::new(
                lit.span(),
                format!(
                    "a skew factor must be finite and greater than zero, but it is {parsed}\n  \
                     below 1 gives more resolution near the minimum, above 1 near the maximum"
                ),
            ));
        }
        return Ok(Curve::Skewed(LitFloat::new(
            &format!("{factor}f64"),
            lit.span(),
        )));
    }
    Err(Error::new(
        lit.span(),
        format!(
            "unknown curve `{trimmed}`\n  valid curves: \"linear\", \"log\", \"skew(2.0)\", \
             \"stepped\""
        ),
    ))
}

/// Parses the `smoothing = ".."` spelling.
fn parse_smoothing(lit: &LitStr) -> Result<SmoothingSpec> {
    let text = lit.value();
    let trimmed = text.trim();
    if trimmed == "none" {
        return Ok(SmoothingSpec::None);
    }
    for (prefix, exponential) in [("linear(", false), ("exp(", true), ("exponential(", true)] {
        if let Some(ms) = trimmed
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let ms = ms.trim();
            let parsed = ms.parse::<f32>().map_err(|_| {
                Error::new(
                    lit.span(),
                    format!(
                        "`{trimmed}` needs a time in milliseconds\n  write \
                         `smoothing = \"exponential(20.0)\"`"
                    ),
                )
            })?;
            if !parsed.is_finite() || parsed < 0.0 {
                return Err(Error::new(
                    lit.span(),
                    format!("a smoothing time must be finite and not negative, but it is {parsed}"),
                ));
            }
            let lit = LitFloat::new(&format!("{ms}f32"), lit.span());
            return Ok(if exponential {
                SmoothingSpec::Exponential(lit)
            } else {
                SmoothingSpec::Linear(lit)
            });
        }
    }
    Err(Error::new(
        lit.span(),
        format!(
            "unknown smoothing `{trimmed}`\n  valid values: \"none\", \"linear(20.0)\", \
             \"exponential(20.0)\""
        ),
    ))
}

/// Extracts a string literal from a list element.
fn string_literal(expr: &Expr, key: &str) -> Result<LitStr> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(lit), ..
        }) => Ok(lit.clone()),
        other => Err(Error::new_spanned(
            other,
            format!("`{key}` takes string literals"),
        )),
    }
}

/// Rejects two fields that would answer to the same id.
///
/// Only literal ids and hashed names can be compared while the macro runs; anything
/// else is checked by the `const` block the expansion emits.
fn check_unique_ids(struct_ident: &Ident, fields: &[ParamField]) -> Result<()> {
    for (index, field) in fields.iter().enumerate() {
        for earlier in &fields[..index] {
            let clash = match (&field.id, &earlier.id) {
                (ParamIdSpec::Literal { value: a, .. }, ParamIdSpec::Literal { value: b, .. })
                    if a == b =>
                {
                    Some(format!("id {a}"))
                }
                (ParamIdSpec::Name(a), ParamIdSpec::Name(b)) if a.value() == b.value() => {
                    Some(format!("id \"{}\"", a.value()))
                }
                _ => None,
            };
            let Some(clash) = clash else {
                continue;
            };
            let mut error = Error::new(
                field.id.span(),
                format!(
                    "duplicate parameter {clash} on `{struct_ident}`: field `{}` already uses \
                     it\n  parameter ids are permanent and must be unique — a repeat makes \
                     every saved project ambiguous",
                    earlier.ident
                ),
            );
            error.combine(Error::new(
                earlier.id.span(),
                format!("`{}` first uses this id here", earlier.ident),
            ));
            return Err(error);
        }
    }
    Ok(())
}

// --------------------------------------------------------------------- generation ---

/// Generates the `Params` impl and, when possible, an inherent `new()`.
fn expand(input: &ParamsInput) -> TokenStream {
    let ParamsInput {
        ident,
        generics,
        krate,
        state_schema_version,
        migrations,
        fields,
        ..
    } = input;
    let private = quote!(#krate::__private);
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let refs = fields.iter().map(|field| {
        let name = &field.ident;
        let id = id_expr(&private, &field.id);
        quote_spanned! { name.span() =>
            (#id, &self.#name as &dyn #private::Param)
        }
    });

    let lookup = lookup_body(&private, fields);

    let schema = state_schema_version.as_ref().map(|version| {
        quote! {
            fn state_schema_version(&self) -> u32 {
                #version
            }
        }
    });

    let migrations = migrations.as_ref().map(|path| {
        quote! {
            fn migrations(&self) -> &[#private::ParamMigration] {
                &#path[..]
            }
        }
    });

    let guard = duplicate_id_guard(&private, ident, fields);
    let constructor = constructor(input, &private);

    quote! {
        #guard

        #[automatically_derived]
        impl #impl_generics #private::Params for #ident #ty_generics #where_clause {
            fn param_refs(&self) -> ::std::vec::Vec<(#private::ParamId, &dyn #private::Param)> {
                ::std::vec![ #(#refs),* ]
            }

            #lookup

            #schema

            #migrations
        }

        #constructor
    }
}

/// The `ParamId` expression for one field.
fn id_expr(private: &TokenStream, id: &ParamIdSpec) -> TokenStream {
    match id {
        ParamIdSpec::Literal { value, span } => {
            let lit = LitInt::new(&format!("{value}u32"), *span);
            quote_spanned!(*span => #private::ParamId::new(#lit))
        }
        ParamIdSpec::Name(name) => {
            quote_spanned!(name.span() => #private::ParamId::from_name(#name))
        }
        ParamIdSpec::Expr(expr) => {
            quote_spanned!(expr.span() => #private::ParamId::new(#expr))
        }
    }
}

/// The raw `u32` expression for one field, usable in a `const`.
fn id_u32(private: &TokenStream, id: &ParamIdSpec) -> TokenStream {
    match id {
        ParamIdSpec::Literal { value, span } => {
            LitInt::new(&format!("{value}u32"), *span).into_token_stream()
        }
        ParamIdSpec::Name(name) => {
            quote_spanned!(name.span() => #private::ParamId::from_name(#name).get())
        }
        ParamIdSpec::Expr(expr) => quote_spanned!(expr.span() => #expr),
    }
}

/// `Params::param`: a match on the raw id when every id is a literal, and a chain of
/// comparisons against `const`-bound ids otherwise. Both are allocation-free, so the
/// audio thread may call it.
fn lookup_body(private: &TokenStream, fields: &[ParamField]) -> TokenStream {
    if fields.is_empty() {
        return quote! {
            fn param(&self, _id: #private::ParamId) -> ::core::option::Option<&dyn #private::Param> {
                ::core::option::Option::None
            }
        };
    }

    if fields.iter().all(|field| field.id.is_literal()) {
        let arms = fields.iter().map(|field| {
            let name = &field.ident;
            let pattern = id_u32(private, &field.id);
            quote_spanned! { name.span() =>
                #pattern => ::core::option::Option::Some(&self.#name as &dyn #private::Param)
            }
        });
        return quote! {
            fn param(&self, id: #private::ParamId) -> ::core::option::Option<&dyn #private::Param> {
                match id.get() {
                    #(#arms,)*
                    _ => ::core::option::Option::None,
                }
            }
        };
    }

    // At least one id is a constant expression, which cannot appear in a pattern. The
    // ids are bound to one `const` array so every comparison is still a compile-time
    // value rather than a call on the audio thread.
    let count = fields.len();
    let ids = fields.iter().map(|field| id_u32(private, &field.id));
    let arms = fields.iter().enumerate().map(|(index, field)| {
        let name = &field.ident;
        let index = syn::Index::from(index);
        quote_spanned! { name.span() =>
            if raw == IDS[#index] {
                return ::core::option::Option::Some(&self.#name as &dyn #private::Param);
            }
        }
    });
    quote! {
        fn param(&self, id: #private::ParamId) -> ::core::option::Option<&dyn #private::Param> {
            const IDS: [u32; #count] = [ #(#ids),* ];
            let raw = id.get();
            #(#arms)*
            ::core::option::Option::None
        }
    }
}

/// A `const` block that rejects duplicate ids the macro could not compare itself.
fn duplicate_id_guard(
    private: &TokenStream,
    ident: &Ident,
    fields: &[ParamField],
) -> Option<TokenStream> {
    if fields.len() < 2 || fields.iter().all(|field| field.id.is_literal()) {
        return None;
    }
    let count = fields.len();
    let ids = fields.iter().map(|field| id_u32(private, &field.id));
    let message = format!(
        "two parameters of `{ident}` share one id — parameter ids must be unique, and \
         `ParamId::from_name` can collide"
    );
    Some(quote! {
        // Ids that are constants rather than literals are only known to the compiler, so
        // the uniqueness check runs there. This costs nothing at run time.
        const _: () = {
            const IDS: [u32; #count] = [ #(#ids),* ];
            let mut i = 0usize;
            while i < #count {
                let mut j = i + 1usize;
                while j < #count {
                    if IDS[i] == IDS[j] {
                        ::core::panic!(#message);
                    }
                    j += 1;
                }
                i += 1;
            }
        };
    })
}

/// The inherent `new()`, generated only when every field of the struct is a parameter
/// that can be built from its attributes.
///
/// A skipped field, an unannotated field or a field carrying only `id` means a generated
/// `Self { .. }` would be missing an initialiser or a value the macro cannot invent, so
/// the author writes `new()` themselves.
fn constructor(input: &ParamsInput, private: &TokenStream) -> Option<TokenStream> {
    let ParamsInput {
        ident,
        vis,
        generics,
        fields,
        every_field_is_a_param,
        ..
    } = input;
    if !every_field_is_a_param || fields.is_empty() {
        return None;
    }
    if fields.iter().any(|field| field.build.is_none()) {
        return None;
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let initialisers = fields.iter().map(|field| {
        let name = &field.ident;
        let build = field
            .build
            .as_ref()
            .unwrap_or_else(|| unreachable!("every field was just checked to be buildable"));
        let value = build_expr(private, &field.id, build);
        quote_spanned!(name.span() => #name: #value)
    });

    let doc = format!(
        "`[main-thread]` Builds every parameter of `{ident}` from its `#[param(..)]` \
         attribute.\n\nGenerated by `#[derive(DauxParams)]`. Allocates the names, units \
         and groups once, while the plug-in is being constructed; nothing here may run on \
         the audio thread. Write this function by hand instead whenever a parameter needs \
         a custom formatter, parser or any other setting the attribute does not cover."
    );

    Some(quote! {
        #[automatically_derived]
        #[allow(clippy::new_without_default)]
        impl #impl_generics #ident #ty_generics #where_clause {
            #[doc = #doc]
            #[must_use]
            #vis fn new() -> Self {
                Self {
                    #(#initialisers,)*
                }
            }
        }
    })
}

/// The constructor call for one parameter.
fn build_expr(private: &TokenStream, id: &ParamIdSpec, build: &Build) -> TokenStream {
    let id = id_expr(private, id);
    let name = &build.name;
    let mut expr = match build.kind {
        ParamKind::Float => {
            let default = build
                .default
                .as_ref()
                .map_or_else(|| quote!(0f64), attr::as_f64_expr);
            let range = range_expr(private, build.range.as_ref());
            quote!(#private::FloatParam::new(#id, #name, #default, #range))
        }
        ParamKind::Int => {
            let default = build
                .default
                .as_ref()
                .map_or_else(|| quote!(0), ToTokens::to_token_stream);
            let (min, max) = match build.range.as_ref() {
                Some(range) => (range.min.to_token_stream(), range.max.to_token_stream()),
                None => (quote!(0), quote!(0)),
            };
            quote!(#private::IntParam::new(#id, #name, #default, #min, #max))
        }
        ParamKind::Bool => {
            let default = build
                .default
                .as_ref()
                .map_or_else(|| quote!(false), ToTokens::to_token_stream);
            quote!(#private::BoolParam::new(#id, #name, #default))
        }
        ParamKind::Enum => {
            let default = build
                .default
                .as_ref()
                .map_or_else(TokenStream::new, ToTokens::to_token_stream);
            quote!(#private::EnumParam::new(#id, #name, #default))
        }
        ParamKind::Meter => {
            let range = range_expr(private, build.range.as_ref());
            quote!(#private::MeterParam::new(#id, #name, #range))
        }
    };

    if let Some(unit) = &build.unit {
        expr = quote!(#expr.with_unit(#unit));
    }
    if let Some(group) = &build.group {
        expr = quote!(#expr.with_group(#group));
    }
    if let Some((off, on)) = &build.labels {
        expr = quote!(#expr.with_labels(#off, #on));
    }
    if let Some(decimals) = &build.decimals {
        expr = quote!(#expr.with_decimals(#decimals));
    }
    if let Some(smoothing) = &build.smoothing {
        let smoothing = match smoothing {
            SmoothingSpec::None => quote!(#private::Smoothing::None),
            SmoothingSpec::Linear(ms) => quote!(#private::Smoothing::Linear { ms: #ms }),
            SmoothingSpec::Exponential(ms) => {
                quote!(#private::Smoothing::Exponential { ms: #ms })
            }
        };
        expr = quote!(#expr.with_smoothing(#smoothing));
    }
    if let Some(flags) = &build.flags {
        let flags = flags_expr(private, flags);
        expr = quote!(#expr.with_flags(#flags));
    }
    expr
}

/// The `ParamRange` expression for a range and its curve.
fn range_expr(private: &TokenStream, range: Option<&RangeSpec>) -> TokenStream {
    let Some(range) = range else {
        return quote!(#private::ParamRange::UNIT);
    };
    match &range.curve {
        Curve::Linear => {
            let (min, max) = (attr::as_f64_expr(&range.min), attr::as_f64_expr(&range.max));
            quote!(#private::ParamRange::Linear { min: #min, max: #max })
        }
        Curve::Logarithmic => {
            let (min, max) = (attr::as_f64_expr(&range.min), attr::as_f64_expr(&range.max));
            quote!(#private::ParamRange::Logarithmic { min: #min, max: #max })
        }
        Curve::Skewed(factor) => {
            let (min, max) = (attr::as_f64_expr(&range.min), attr::as_f64_expr(&range.max));
            quote!(#private::ParamRange::Skewed { min: #min, max: #max, factor: #factor })
        }
        Curve::Stepped => {
            let (min, max) = (&range.min, &range.max);
            quote!(#private::ParamRange::Stepped { min: #min, max: #max })
        }
    }
}

/// The `ParamFlags` expression for a list of flag names.
fn flags_expr(private: &TokenStream, flags: &[Ident]) -> TokenStream {
    if flags.is_empty() {
        return quote!(#private::ParamFlags::EMPTY);
    }
    let constants = flags.iter().map(|flag| {
        let written = flag.to_string();
        let constant = FLAGS
            .iter()
            .find(|(name, _)| *name == written)
            .map(|(_, constant)| *constant)
            .unwrap_or_else(|| unreachable!("flag names were validated during parsing"));
        let constant = Ident::new(constant, flag.span());
        quote_spanned!(flag.span() => #private::ParamFlags::#constant)
    });
    quote!(#(#constants)|*)
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
        struct GainParams {
            #[param(id = 1, name = "Gain", range = -60.0..=12.0, unit = "dB", default = 0.0,
                    smoothing = "exponential(20.0)", decimals = 1, group = "Output",
                    flags(automatable, modulatable))]
            gain: FloatParam,
            #[param(id = 2, name = "Invert")]
            invert: BoolParam,
        }
    "#;

    #[test]
    fn generates_a_params_impl_in_declaration_order() {
        let expanded = expand_str(GAIN);
        assert!(expanded.contains("impl :: daux_plugin :: __private :: Params for GainParams"));
        let gain = expanded.find("Gain").expect("gain is generated");
        let invert = expanded.find("Invert").expect("invert is generated");
        assert!(gain < invert, "declaration order must be preserved");
    }

    #[test]
    fn the_lookup_is_a_match_on_the_raw_id() {
        let expanded = expand_str(GAIN);
        assert!(expanded.contains("match id . get ()"), "{expanded}");
        assert!(expanded.contains("1u32 =>"), "{expanded}");
        assert!(expanded.contains("2u32 =>"), "{expanded}");
        // No allocation, no table, no lazy initialisation on the audio thread.
        assert!(!expanded.contains("Vec :: from"), "{expanded}");
        assert!(!expanded.contains("HashMap"), "{expanded}");
    }

    #[test]
    fn builds_every_parameter_from_its_attribute() {
        let expanded = expand_str(GAIN);
        assert!(expanded.contains("fn new () -> Self"), "{expanded}");
        assert!(
            // `default = 0.0` is already a float literal, so it is emitted unchanged;
            // only integer literals are rewritten (see `attr::as_f64_expr`).
            expanded.contains("FloatParam :: new (:: daux_plugin :: __private :: ParamId :: new (1u32) , \"Gain\" , 0.0 ,"),
            "{expanded}"
        );
        assert!(expanded.contains("with_unit (\"dB\")"), "{expanded}");
        assert!(expanded.contains("with_decimals (1)"), "{expanded}");
        assert!(expanded.contains("with_group (\"Output\")"), "{expanded}");
        assert!(
            expanded.contains("Smoothing :: Exponential { ms : 20.0f32 }"),
            "{expanded}"
        );
        assert!(
            expanded.contains(
                "ParamFlags :: AUTOMATABLE | :: daux_plugin :: __private :: ParamFlags :: MODULATABLE"
            ),
            "{expanded}"
        );
        assert!(
            expanded.contains("BoolParam :: new (:: daux_plugin :: __private :: ParamId :: new (2u32) , \"Invert\" , false)"),
            "{expanded}"
        );
    }

    #[test]
    fn a_field_that_only_carries_an_id_suppresses_the_constructor() {
        let expanded = expand_str(
            r"
            struct Bank {
                #[param(id = 1)]
                gain: FloatParam,
            }
            ",
        );
        assert!(expanded.contains("fn param_refs"), "{expanded}");
        assert!(!expanded.contains("fn new ()"), "{expanded}");
    }

    #[test]
    fn a_skipped_field_is_not_a_parameter_and_suppresses_the_constructor() {
        let expanded = expand_str(
            r#"
            struct Bank {
                #[param(id = 1, name = "Gain", range = 0.0..=1.0, default = 1.0)]
                gain: FloatParam,
                #[param(skip)]
                cached: std::sync::Arc<Table>,
                unannotated: u32,
            }
            "#,
        );
        assert!(!expanded.contains("cached"), "{expanded}");
        assert!(!expanded.contains("unannotated"), "{expanded}");
        assert!(!expanded.contains("fn new ()"), "{expanded}");
    }

    #[test]
    fn one_unannotated_field_is_enough_to_suppress_the_constructor() {
        // Regression: a generated `Self { gain: .. }` would be missing an initialiser for
        // `cache`, so it must not be generated at all. The trait impl is unaffected.
        let expanded = expand_str(
            r#"
            struct Bank {
                #[param(id = 1, name = "Gain", range = 0.0..=1.0, default = 1.0)]
                gain: FloatParam,
                cache: Vec<f32>,
            }
            "#,
        );
        assert!(expanded.contains("fn param_refs"), "{expanded}");
        assert!(expanded.contains("1u32 =>"), "{expanded}");
        assert!(!expanded.contains("fn new ()"), "{expanded}");
        assert!(!expanded.contains("cache"), "{expanded}");
    }

    #[test]
    fn a_bare_param_attribute_explains_the_syntax() {
        let message = error("struct Bank { #[param] gain: FloatParam }");
        assert!(message.contains("needs arguments"), "{message}");
    }

    #[test]
    fn an_empty_bank_still_implements_the_trait() {
        let expanded = expand_str("struct Empty {}");
        assert!(expanded.contains("fn param (& self , _id"), "{expanded}");
        assert!(expanded.contains(":: std :: vec ! []"), "{expanded}");
    }

    #[test]
    fn the_container_attribute_passes_the_schema_version_and_migrations_through() {
        let expanded = expand_str(
            r#"
            #[params(state_schema_version = 3, migrations = MIGRATIONS)]
            struct Bank {
                #[param(id = 1)]
                gain: FloatParam,
            }
            "#,
        );
        assert!(
            expanded.contains("fn state_schema_version (& self) -> u32 { 3 }"),
            "{expanded}"
        );
        assert!(
            expanded.contains("fn migrations (& self) -> & [:: daux_plugin :: __private :: ParamMigration] { & MIGRATIONS [..] }"),
            "{expanded}"
        );
    }

    #[test]
    fn the_crate_path_can_be_redirected() {
        let expanded = expand_str(
            r#"
            #[params(crate = ::daux_plugin_api)]
            struct Bank {
                #[param(id = 1)]
                gain: FloatParam,
            }
            "#,
        );
        assert!(
            expanded.contains(":: daux_plugin_api :: __private :: Params"),
            "{expanded}"
        );
    }

    #[test]
    fn named_ids_are_hashed_and_guarded_at_compile_time() {
        let expanded = expand_str(
            r#"
            struct Bank {
                #[param(id = "gain")]
                gain: FloatParam,
                #[param(id = MY_ID)]
                other: FloatParam,
            }
            "#,
        );
        assert!(
            expanded.contains("ParamId :: from_name (\"gain\")"),
            "{expanded}"
        );
        // Non-literal ids cannot be patterns, so the lookup compares against consts.
        assert!(
            expanded.contains("const IDS : [u32 ; 2usize]"),
            "{expanded}"
        );
        assert!(expanded.contains("if raw == IDS [0]"), "{expanded}");
        // …and duplicates are caught by the compiler instead of the macro.
        assert!(expanded.contains("const _ : () ="), "{expanded}");
        assert!(expanded.contains(":: core :: panic !"), "{expanded}");
    }

    #[test]
    fn literal_ids_need_no_const_guard() {
        let expanded = expand_str(GAIN);
        assert!(!expanded.contains("const _ : () ="), "{expanded}");
    }

    #[test]
    fn every_parameter_type_is_constructible() {
        let expanded = expand_str(
            r#"
            struct All {
                #[param(id = 1, name = "Cutoff", range = 20.0..=20000.0, default = 1000.0,
                        curve = "log", unit = "Hz")]
                cutoff: FloatParam,
                #[param(id = 2, name = "Voices", range = 1..=16, default = 8, unit = "voices")]
                voices: IntParam,
                #[param(id = 3, name = "Invert", default = true, labels("Normal", "Inverted"))]
                invert: BoolParam,
                #[param(id = 4, name = "Shape", default = Shape::Sine)]
                shape: EnumParam<Shape>,
                #[param(id = 5, name = "Level", range = -60.0..=6.0, unit = "dB", flags(is_meter, read_only))]
                level: MeterParam,
            }
            "#,
        );
        assert!(expanded.contains("ParamRange :: Logarithmic"), "{expanded}");
        assert!(
            expanded.contains("IntParam :: new (:: daux_plugin :: __private :: ParamId :: new (2u32) , \"Voices\" , 8 , 1 , 16)"),
            "{expanded}"
        );
        assert!(
            expanded.contains("with_labels (\"Normal\" , \"Inverted\")"),
            "{expanded}"
        );
        assert!(expanded.contains("EnumParam :: new"), "{expanded}");
        assert!(expanded.contains("Shape :: Sine"), "{expanded}");
        assert!(expanded.contains("MeterParam :: new"), "{expanded}");
    }

    #[test]
    fn integer_bounds_on_a_float_parameter_become_floats() {
        let expanded = expand_str(
            r#"
            struct Bank {
                #[param(id = 1, name = "Mix", range = 0..=1, default = 1)]
                mix: FloatParam,
            }
            "#,
        );
        assert!(
            expanded.contains("Linear { min : 0f64 , max : 1f64 }"),
            "{expanded}"
        );
        assert!(expanded.contains("\"Mix\" , 1f64"), "{expanded}");
    }

    #[test]
    fn skewed_curves_carry_their_factor() {
        let expanded = expand_str(
            r#"
            struct Bank {
                #[param(id = 1, name = "Drive", range = 0.0..=1.0, default = 0.5, curve = "skew(0.3)")]
                drive: FloatParam,
            }
            "#,
        );
        assert!(
            expanded.contains("Skewed { min : 0.0 , max : 1.0 , factor : 0.3f64 }"),
            "{expanded}"
        );
    }

    #[test]
    fn generics_are_carried_through() {
        let expanded = expand_str(
            r#"
            struct Bank<E: ParamEnum> {
                #[param(id = 1, name = "Shape", default = E::default_shape())]
                shape: EnumParam<E>,
            }
            "#,
        );
        assert!(
            expanded.contains(
                "impl < E : ParamEnum > :: daux_plugin :: __private :: Params for Bank < E >"
            ),
            "{expanded}"
        );
    }

    // ------------------------------------------------------------------- errors ---

    #[test]
    fn a_missing_id_says_what_to_write() {
        let message = error(
            r#"
            struct Bank {
                #[param(name = "Gain")]
                gain: FloatParam,
            }
            "#,
        );
        assert!(
            message.contains("field `gain` has no parameter id"),
            "{message}"
        );
        assert!(message.contains("#[param(id = 1"), "{message}");
        assert!(message.contains("#[param(skip)]"), "{message}");
    }

    #[test]
    fn duplicate_literal_ids_name_both_fields() {
        let message = error(
            r#"
            struct Bank {
                #[param(id = 7)]
                gain: FloatParam,
                #[param(id = 7)]
                mix: FloatParam,
            }
            "#,
        );
        assert!(
            message.contains("duplicate parameter id 7 on `Bank`: field `gain` already uses it"),
            "{message}"
        );
        assert!(
            message.contains("`gain` first uses this id here"),
            "{message}"
        );
    }

    #[test]
    fn duplicate_named_ids_name_both_fields() {
        let message = error(
            r#"
            struct Bank {
                #[param(id = "gain")]
                gain: FloatParam,
                #[param(id = "gain")]
                other: FloatParam,
            }
            "#,
        );
        assert!(
            message.contains("duplicate parameter id \"gain\""),
            "{message}"
        );
    }

    #[test]
    fn an_id_that_is_not_a_number_or_a_name_is_rejected() {
        let message = error(
            r"
            struct Bank {
                #[param(id = 1.5)]
                gain: FloatParam,
            }
            ",
        );
        assert!(
            message.contains("`id` must be a `u32` literal"),
            "{message}"
        );
    }

    #[test]
    fn an_id_that_does_not_fit_in_a_u32_is_rejected() {
        let message = error(
            r"
            struct Bank {
                #[param(id = 99999999999999)]
                gain: FloatParam,
            }
            ",
        );
        assert!(message.contains("number too large"), "{message}");
    }

    #[test]
    fn a_half_open_range_is_rejected() {
        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Gain", range = -60.0..12.0, default = 0.0)]
                gain: FloatParam,
            }
            "#,
        );
        assert!(message.contains("inclusive range"), "{message}");
        assert!(message.contains("`..=`, not `..`"), "{message}");
    }

    #[test]
    fn an_inverted_or_empty_range_is_rejected() {
        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Gain", range = 12.0..=-60.0, default = 0.0)]
                gain: FloatParam,
            }
            "#,
        );
        assert!(message.contains("inverted"), "{message}");

        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Gain", range = 1.0..=1.0, default = 1.0)]
                gain: FloatParam,
            }
            "#,
        );
        assert!(message.contains("must differ"), "{message}");
    }

    #[test]
    fn a_logarithmic_range_from_zero_is_rejected_at_compile_time() {
        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Cutoff", range = 0.0..=20000.0, default = 1000.0, curve = "log")]
                cutoff: FloatParam,
            }
            "#,
        );
        assert!(message.contains("strictly positive bounds"), "{message}");
        assert!(message.contains("20.0..=20000.0"), "{message}");
    }

    #[test]
    fn a_default_outside_the_range_is_rejected() {
        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Gain", range = -60.0..=12.0, default = 24.0)]
                gain: FloatParam,
            }
            "#,
        );
        assert!(
            message.contains("default 24 is outside the range -60..=12"),
            "{message}"
        );
    }

    #[test]
    fn a_fractional_bound_on_a_stepped_range_is_rejected() {
        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Voices", range = 1.0..=16.0, default = 8)]
                voices: IntParam,
            }
            "#,
        );
        assert!(message.contains("whole numbers"), "{message}");
    }

    #[test]
    fn an_unknown_curve_lists_the_valid_ones() {
        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Gain", range = 0.0..=1.0, default = 0.0, curve = "expo")]
                gain: FloatParam,
            }
            "#,
        );
        assert!(message.contains("unknown curve `expo`"), "{message}");
        assert!(message.contains("\"skew(2.0)\""), "{message}");
    }

    #[test]
    fn a_broken_skew_factor_is_rejected() {
        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Gain", range = 0.0..=1.0, default = 0.0, curve = "skew(0)")]
                gain: FloatParam,
            }
            "#,
        );
        assert!(message.contains("greater than zero"), "{message}");

        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Gain", range = 0.0..=1.0, default = 0.0, curve = "skew(x)")]
                gain: FloatParam,
            }
            "#,
        );
        assert!(message.contains("needs a number"), "{message}");
    }

    #[test]
    fn an_unknown_smoothing_lists_the_valid_ones() {
        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Gain", range = 0.0..=1.0, default = 0.0, smoothing = "fast")]
                gain: FloatParam,
            }
            "#,
        );
        assert!(message.contains("unknown smoothing `fast`"), "{message}");
        assert!(message.contains("\"exponential(20.0)\""), "{message}");
    }

    #[test]
    fn an_unknown_flag_suggests_the_nearest_one() {
        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Gain", range = 0.0..=1.0, default = 0.0, flags(automatible))]
                gain: FloatParam,
            }
            "#,
        );
        assert!(
            message.contains("unknown parameter flag `automatible`"),
            "{message}"
        );
        assert!(message.contains("did you mean `automatable`?"), "{message}");
    }

    #[test]
    fn keys_that_do_not_apply_to_the_type_explain_themselves() {
        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Invert", range = 0.0..=1.0)]
                invert: BoolParam,
            }
            "#,
        );
        assert!(
            message.contains("`range` does not apply here: a BoolParam is off or on"),
            "{message}"
        );

        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Level", range = 0.0..=1.0, default = 0.5)]
                level: MeterParam,
            }
            "#,
        );
        assert!(
            message.contains("`default` does not apply here"),
            "{message}"
        );

        // `labels` only exists on `BoolParam`; emitting `.with_labels(..)` on any other
        // parameter type would be a type error in the author's crate, far from the cause.
        for (ty, extra) in [
            ("FloatParam", "range = 0.0..=1.0, default = 0.0, "),
            ("IntParam", "range = 0..=1, default = 0, "),
            ("EnumParam<Shape>", "default = Shape::Sine, "),
            ("MeterParam", "range = 0.0..=1.0, "),
        ] {
            let source = format!(
                r#"struct Bank {{ #[param(id = 1, name = "X", {extra}labels("a", "b"))] x: {ty} }}"#
            );
            let message = error(&source);
            assert!(
                message.contains("`labels` does not apply here"),
                "`labels` on a {ty} must be rejected, got: {message}"
            );
        }
    }

    #[test]
    fn a_missing_range_or_default_is_reported() {
        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Gain")]
                gain: FloatParam,
            }
            "#,
        );
        assert!(message.contains("needs a range"), "{message}");

        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Gain", range = 0.0..=1.0)]
                gain: FloatParam,
            }
            "#,
        );
        assert!(message.contains("needs a default value"), "{message}");
    }

    #[test]
    fn an_unknown_parameter_type_points_at_the_manual_path() {
        let message = error(
            r#"
            struct Bank {
                #[param(id = 1, name = "Gain")]
                gain: MyOwnParam,
            }
            "#,
        );
        assert!(
            message.contains("cannot build `MyOwnParam` from attributes"),
            "{message}"
        );
        assert!(
            message.contains("build it in your own `new()`"),
            "{message}"
        );
    }

    #[test]
    fn skip_cannot_be_combined_with_other_keys() {
        let message = error(
            r"
            struct Bank {
                #[param(skip, id = 1)]
                gain: FloatParam,
            }
            ",
        );
        assert!(
            message.contains("cannot be combined with `skip`"),
            "{message}"
        );
    }

    #[test]
    fn a_tuple_struct_or_enum_is_rejected_with_the_shape_to_write() {
        let message = error("struct Bank(FloatParam);");
        assert!(message.contains("needs named fields"), "{message}");

        let message = error("enum Bank { A }");
        assert!(
            message.contains("needs a struct with named fields"),
            "{message}"
        );
    }

    #[test]
    fn an_unknown_container_key_is_rejected() {
        let message = error(
            r"
            #[params(schema_version = 2)]
            struct Bank {
                #[param(id = 1)]
                gain: FloatParam,
            }
            ",
        );
        assert!(message.contains("unknown `#[params(..)]` key"), "{message}");
        assert!(message.contains("state_schema_version"), "{message}");
    }
}

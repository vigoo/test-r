use syn::{FnArg, ItemFn};

pub fn is_testr_attribute(attr: &syn::Attribute, name: &str) -> bool {
    let path = attr.path();
    if path.is_ident(name) {
        true
    } else if path
        .segments
        .last()
        .map(|segment| segment.ident == name)
        .unwrap_or(false)
    {
        if path.segments.len() == 2 {
            path.segments.first().unwrap().ident == "test_r"
        } else if path.segments.len() == 3 {
            path.segments.get(0).unwrap().ident == ""
                && path.segments.get(1).unwrap().ident == "test_r"
        } else {
            false
        }
    } else {
        false
    }
}

pub fn filter_custom_parameter_attributes(ast: &mut ItemFn) {
    ast.sig.inputs.iter_mut().for_each(|param| {
        if let FnArg::Typed(typed) = param {
            typed.attrs.retain(|attr| {
                !is_testr_attribute(attr, "tagged_as") && !is_testr_attribute(attr, "dimension")
            });
        }
    });
}

/// Parses a human-readable duration such as `"1s"`, `"500ms"`, `"1h 30m"` or
/// `"2 days"` into whole milliseconds. Days count as 24 hours and weeks as 7 days;
/// months, years and negative durations are rejected.
pub fn parse_duration_millis(value: &str) -> Option<u64> {
    let span: jiff::Span = value.parse().ok()?;
    if span.is_negative() {
        return None;
    }
    let millis = span
        .total(jiff::SpanTotal::from(jiff::Unit::Millisecond).days_are_24_hours())
        .ok()?;
    Some(millis as u64)
}

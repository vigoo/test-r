use syn::{FnArg, ItemFn};

pub fn parse_timeout_duration(input: &str) -> Result<std::time::Duration, String> {
    use fundu::{CustomDurationParser, CustomTimeUnit, Multiplier, TimeUnit::*};

    let parser = CustomDurationParser::builder()
        .time_units(&[
            CustomTimeUnit::with_default(NanoSecond, &["nanos", "nsec", "ns"]),
            CustomTimeUnit::with_default(MicroSecond, &["usec", "us", "µs"]),
            CustomTimeUnit::with_default(MilliSecond, &["millis", "msec", "ms"]),
            CustomTimeUnit::with_default(Second, &["seconds", "second", "secs", "sec", "s"]),
            CustomTimeUnit::with_default(Minute, &["minutes", "minute", "min", "mins", "m"]),
            CustomTimeUnit::with_default(Hour, &["hours", "hour", "hr", "hrs", "h"]),
            CustomTimeUnit::with_default(Day, &["days", "day", "d"]),
            CustomTimeUnit::with_default(Week, &["weeks", "week", "wk", "wks", "w"]),
            // Preserve humantime's 30.44-day month and 365.25-day year.
            CustomTimeUnit::new(
                Second,
                &["months", "month", "M"],
                Some(Multiplier(2_630_016, 0)),
            ),
            CustomTimeUnit::new(
                Second,
                &["years", "year", "yr", "yrs", "y"],
                Some(Multiplier(31_557_600, 0)),
            ),
        ])
        .allow_time_unit_delimiter()
        .parse_multiple(None)
        .build();
    let duration: std::time::Duration = parser
        .parse(input.trim())
        .map_err(|error| error.to_string())?
        .try_into()
        .map_err(|error: fundu::TryFromDurationError| error.to_string())?;
    // The generated timeout is stored as u64 milliseconds; never wrap it.
    u64::try_from(duration.as_millis())
        .map_err(|_| "timeout exceeds u64 milliseconds".to_owned())?;
    Ok(duration)
}

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

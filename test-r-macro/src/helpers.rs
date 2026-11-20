use syn::{FnArg, ItemFn};

pub fn parse_timeout_duration(input: &str) -> Result<std::time::Duration, String> {
    use fundu::{CustomDurationParser, CustomTimeUnit, Multiplier, TimeUnit::*};

    if input == "0" {
        return Ok(std::time::Duration::ZERO);
    }
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
        .disable_exponent()
        .disable_infinity()
        .build();
    let invalid = || "invalid duration or duration overflow".to_owned();
    let mut remaining = input.trim();
    let mut total = std::time::Duration::ZERO;
    if remaining.is_empty() {
        return Err(invalid());
    }
    while !remaining.is_empty() {
        // Keep humantime's number grammar (including Unicode whitespace inside
        // numbers), rather than fundu's optional units, signs and exponents.
        let number_end = remaining
            .find(|c: char| !c.is_ascii_digit() && c != '.' && !c.is_whitespace())
            .ok_or_else(invalid)?;
        let number: String = remaining[..number_end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        remaining = &remaining[number_end..];
        let unit_end = remaining
            .find(|c: char| !c.is_ascii_alphabetic() && c != 'µ')
            .unwrap_or(remaining.len());
        let unit = &remaining[..unit_end];
        remaining = remaining[unit_end..].trim_start();
        let (whole, fraction) = number
            .split_once('.')
            .map_or((number.as_str(), None), |(w, f)| (w, Some(f)));
        let whole = whole.parse::<u64>().map_err(|_| invalid())?;
        let scale = parser
            .parse(&format!("1{unit}"))
            .map_err(|error| error.to_string())?;
        // humantime checks integer arithmetic in nanoseconds for subsecond
        // units, seconds otherwise. It requires fractional hours and larger
        // units to be exact seconds, and smaller fractions to be exact nanos.
        let integer_scale = if scale.as_seconds() == 0 {
            scale.as_nanos()
        } else {
            scale.as_seconds()
        } as u64;
        let integer = whole.checked_mul(integer_scale).ok_or_else(invalid)?;
        if scale.as_seconds() == 0 {
            // Preserve the old parser's checked addition before normalizing
            // a subsecond component into seconds and nanoseconds.
            (total.subsec_nanos() as u64)
                .checked_add(integer)
                .ok_or_else(invalid)?;
        }
        let mut duration = parser
            .parse(&format!("{whole}{unit}"))
            .map_err(|error| error.to_string())?;
        if let Some(fraction) = fraction {
            if fraction.is_empty() || scale.as_nanos() == 1 {
                return Err(invalid());
            }
            let numerator = fraction.parse::<u64>().map_err(|_| invalid())?;
            let denominator = 10u64
                .checked_pow(fraction.len().try_into().map_err(|_| invalid())?)
                .ok_or_else(invalid)?;
            let fractional_scale = if scale.as_seconds() >= 3600 {
                scale.as_seconds()
            } else {
                scale.as_nanos()
            } as u64;
            if numerator
                .checked_mul(fractional_scale)
                .ok_or_else(invalid)?
                % denominator
                != 0
            {
                return Err(invalid());
            }
            // Parsing the combined value would silently saturate in fundu.
            let fraction = parser
                .parse(&format!("0.{fraction}{unit}"))
                .map_err(|error| error.to_string())?;
            duration = duration.checked_add(fraction).ok_or_else(invalid)?;
        }
        let duration: std::time::Duration = duration
            .try_into()
            .map_err(|error: fundu::TryFromDurationError| error.to_string())?;
        total = total.checked_add(duration).ok_or_else(invalid)?;
    }
    Ok(total)
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

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

use super::{InstructionError, InstructionKind, InstructionResourceRef, InstructionSelector};

#[derive(Clone, Debug)]
pub(crate) enum TemplateSegment {
    Text(String),
    Expression(String),
    Partial(InstructionSelector),
}

pub(crate) fn parse_restricted_template(
    resource: &InstructionResourceRef,
    source: &str,
) -> Result<Vec<TemplateSegment>, InstructionError> {
    let mut segments = Vec::new();
    let mut cursor = 0usize;
    while let Some(relative_start) = source[cursor..].find("{{") {
        let start = cursor + relative_start;
        if start > cursor {
            segments.push(TemplateSegment::Text(source[cursor..start].to_string()));
        }

        let triple = source[start..].starts_with("{{{");
        let (open_len, close) = if triple {
            (3usize, "}}}")
        } else {
            (2usize, "}}")
        };
        let content_start = start + open_len;
        let Some(relative_end) = source[content_start..].find(close) else {
            return Err(InstructionError::RestrictedTemplate {
                resource: resource.clone(),
                detail: format!("unclosed Handlebars expression at byte {start}"),
            });
        };
        let end = content_start + relative_end;
        let raw = &source[content_start..end];
        let tag = raw.trim();
        if tag.is_empty() {
            return Err(InstructionError::RestrictedTemplate {
                resource: resource.clone(),
                detail: format!("empty Handlebars expression at byte {start}"),
            });
        }

        if !triple && tag.starts_with('!') {
            // Handlebars comments are inert and do not introduce a helper surface.
        } else if !triple && tag.starts_with('>') {
            let reference = tag[1..].trim();
            if reference.is_empty() || reference.split_whitespace().count() != 1 {
                return Err(InstructionError::RestrictedTemplate {
                    resource: resource.clone(),
                    detail: format!(
                        "partial reference '{tag}' must contain exactly one resource ID"
                    ),
                });
            }
            segments.push(TemplateSegment::Partial(InstructionSelector::parse(
                InstructionKind::Module,
                reference,
            )?));
            cursor = end + close.len();
            continue;
        } else {
            validate_value_expression(resource, tag)?;
        }

        segments.push(TemplateSegment::Expression(
            source[start..end + close.len()].to_string(),
        ));
        cursor = end + close.len();
    }

    if cursor < source.len() {
        segments.push(TemplateSegment::Text(source[cursor..].to_string()));
    }
    if segments.is_empty() {
        segments.push(TemplateSegment::Text(String::new()));
    }
    Ok(segments)
}

fn validate_value_expression(
    resource: &InstructionResourceRef,
    expression: &str,
) -> Result<(), InstructionError> {
    if matches!(
        expression.chars().next(),
        Some('#' | '/' | '^' | '&' | '{' | '>')
    ) {
        return Err(InstructionError::RestrictedTemplate {
            resource: resource.clone(),
            detail: format!(
                "helpers, blocks, subexpressions, and unescaped helper forms are not allowed: '{{{{{expression}}}}}'"
            ),
        });
    }
    if expression.split_whitespace().count() != 1 {
        return Err(InstructionError::RestrictedTemplate {
            resource: resource.clone(),
            detail: format!(
                "only one typed value path is allowed per expression: '{{{{{expression}}}}}'"
            ),
        });
    }
    let valid = expression.split('.').all(|part| {
        !part.is_empty()
            && part.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
    });
    if !valid {
        return Err(InstructionError::RestrictedTemplate {
            resource: resource.clone(),
            detail: format!("invalid typed value path '{expression}'"),
        });
    }
    Ok(())
}

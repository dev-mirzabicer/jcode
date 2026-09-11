use anyhow::{Result, bail, ensure};
use jcode_tool_types::presentation::OutputSize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputBinding {
    Flat,
    Wrapped,
}

impl InputBinding {
    pub fn for_schema(schema: &Value) -> Self {
        if schema
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| {
                properties.contains_key("output_size")
                    || properties.contains_key(crate::ACCEPT_LARGE_OUTPUT_KEY)
            })
        {
            Self::Wrapped
        } else {
            Self::Flat
        }
    }

    pub fn for_external_schema(schema: &Value) -> Self {
        if schema.get("additionalProperties") != Some(&Value::Bool(false))
            || schema
                .get("properties")
                .and_then(Value::as_object)
                .is_some_and(|properties| {
                    properties.contains_key("intent")
                        || properties.contains_key("arguments")
                        || properties.contains_key("parameters")
                })
        {
            Self::Wrapped
        } else {
            Self::for_schema(schema)
        }
    }

    pub fn schema(self, producer: Value) -> Value {
        let schema = if self == Self::Wrapped {
            json!({"type":"object", "properties":{"arguments":producer}, "required":["arguments"]})
        } else {
            producer
        };
        crate::ensure_intent_in_schema(schema)
    }

    /// The published binding selects decoding. Never infer a wrapper from the
    /// contents of producer arguments (which may legitimately contain one).
    pub fn decode(self, mut input: Value) -> Result<(Value, Option<OutputSize>)> {
        if input.is_null() {
            input = json!({});
        }
        let object = input
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("Tool input must be an object"))?;
        let size = object
            .remove("output_size")
            .filter(|value| !value.is_null())
            .map(serde_json::from_value)
            .transpose()?;
        match object.remove(crate::ACCEPT_LARGE_OUTPUT_KEY) {
            None | Some(Value::Null) | Some(Value::Bool(false)) => {}
            Some(Value::String(value)) if value.eq_ignore_ascii_case("false") => {}
            Some(Value::Bool(true)) => bail!(
                "accept_large_output reexecution is retired. Read the retained output file from the original result. No tool was executed."
            ),
            Some(Value::String(value)) if value.eq_ignore_ascii_case("true") => bail!(
                "accept_large_output reexecution is retired. Read the retained output file from the original result. No tool was executed."
            ),
            Some(_) => bail!(
                "Invalid legacy accept_large_output value. Use output_size or read a retained output file."
            ),
        }
        if self == Self::Wrapped {
            let arguments = object.remove("arguments").ok_or_else(|| {
                anyhow::anyhow!("This tool requires its producer input in arguments")
            })?;
            ensure!(
                object.keys().all(|key| key == "intent"),
                "Unknown framework field outside arguments"
            );
            Ok((arguments, size))
        } else {
            Ok((input, size))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colliding_producer_fields_survive_wrapped_decoding() -> Result<()> {
        let original = json!({"type":"object","properties":{"output_size":{"type":"boolean"},"accept_large_output":{"type":"string"}},"required":["output_size"]});
        let binding = InputBinding::for_schema(&original);
        assert_eq!(binding, InputBinding::Wrapped);
        let published = binding.schema(original.clone());
        assert_eq!(published["properties"]["arguments"], original);
        let arguments = json!({"output_size":true,"accept_large_output":"producer-owned"});
        let (decoded, size) = binding
            .decode(json!({"arguments":arguments,"output_size":"small","intent":"fixture"}))?;
        assert_eq!(decoded, arguments);
        assert_eq!(size.unwrap().target().get(), 20_000);
        Ok(())
    }

    #[test]
    fn legacy_true_never_reaches_a_producer() {
        for value in [json!(true), json!("true")] {
            assert!(
                InputBinding::Flat
                    .decode(json!({"accept_large_output":value}))
                    .is_err()
            );
        }
        assert!(
            InputBinding::Flat
                .decode(json!({"accept_large_output":false}))
                .is_ok()
        );
        assert!(InputBinding::Flat.decode(json!({})).is_ok());
    }
}

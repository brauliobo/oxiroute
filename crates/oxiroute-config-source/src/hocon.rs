use std::collections::HashMap;

use hocon::parser::{AstField, AstNode};
use hocon::{Token, TokenKind};
use serde_json::Value;

use crate::ConfigSourceError;
use crate::limits::validate_value;

pub(crate) fn decode(source: &str) -> Result<Value, ConfigSourceError> {
    let tokens = hocon::tokenize(source)
        .map_err(|error| ConfigSourceError::parse("HOCON", error.to_string()))?;
    reject_include_tokens(&tokens)?;
    let ast = hocon::parser::parse_tokens(&tokens)
        .map_err(|error| ConfigSourceError::parse("HOCON", error.to_string()))?;
    reject_includes(&ast)?;

    let config = hocon::parse_with_env(source, &HashMap::new())
        .map_err(|error| ConfigSourceError::parse("HOCON", error.to_string()))?;
    let value = config
        .deserialize::<Value>()
        .map_err(|error| ConfigSourceError::parse("HOCON", error.to_string()))?;
    validate_value(&value)?;
    Ok(value)
}

fn reject_include_tokens(tokens: &[Token]) -> Result<(), ConfigSourceError> {
    let mut object_stack = vec![true];
    let mut at_entry_start = true;
    for token in tokens {
        let in_object = object_stack.last().copied().unwrap_or(true);
        if in_object
            && at_entry_start
            && token.kind == TokenKind::Unquoted
            && token.value == "include"
        {
            return Err(ConfigSourceError::parse(
                "HOCON",
                format!(
                    "include directives are forbidden (line {}, column {})",
                    token.line, token.col
                ),
            ));
        }
        match token.kind {
            TokenKind::LBrace => {
                object_stack.push(true);
                at_entry_start = true;
            }
            TokenKind::LBracket => {
                object_stack.push(false);
                at_entry_start = true;
            }
            TokenKind::RBrace | TokenKind::RBracket => {
                object_stack.pop();
                at_entry_start = false;
            }
            TokenKind::Newline | TokenKind::Comma if in_object => at_entry_start = true,
            TokenKind::Eof => {}
            _ => at_entry_start = false,
        }
    }
    Ok(())
}

fn reject_includes(node: &AstNode) -> Result<(), ConfigSourceError> {
    match node {
        AstNode::Include { pos, .. } => Err(ConfigSourceError::parse(
            "HOCON",
            format!(
                "include directives are forbidden (line {}, column {})",
                pos.line, pos.col
            ),
        )),
        AstNode::Object { fields, .. } => reject_fields(fields),
        AstNode::Array { items, .. } | AstNode::Concat { nodes: items, .. } => {
            for item in items {
                reject_includes(item)?;
            }
            Ok(())
        }
        AstNode::Scalar { .. } | AstNode::Substitution { .. } => Ok(()),
    }
}

fn reject_fields(fields: &[AstField]) -> Result<(), ConfigSourceError> {
    for field in fields {
        reject_includes(&field.value)?;
    }
    Ok(())
}

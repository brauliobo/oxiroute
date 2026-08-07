use std::collections::HashMap;

use hocon::parser::{AstField, AstNode};
use hocon::{Token, TokenKind};
use serde_json::Value;

use crate::ConfigSourceError;
use crate::limits::{
    MAX_NODES, MAX_STRUCTURAL_DEPTH, MAX_SUBSTITUTIONS, check_string, validate_value,
};

pub(crate) fn decode(source: &str) -> Result<Value, ConfigSourceError> {
    let tokens = hocon::tokenize(source)
        .map_err(|error| ConfigSourceError::parse("HOCON", error.to_string()))?;
    check_token_budget(&tokens)?;
    reject_include_tokens(&tokens)?;
    let ast = hocon::parser::parse_tokens(&tokens)
        .map_err(|error| ConfigSourceError::parse("HOCON", error.to_string()))?;
    check_ast_budget(&ast)?;
    reject_includes(&ast)?;

    let config = hocon::parse_with_env(source, &HashMap::new())
        .map_err(|error| ConfigSourceError::parse("HOCON", error.to_string()))?;
    let value = config
        .deserialize::<Value>()
        .map_err(|error| ConfigSourceError::parse("HOCON", error.to_string()))?;
    validate_value(&value)?;
    Ok(value)
}

fn check_token_budget(tokens: &[Token]) -> Result<(), ConfigSourceError> {
    let mut delimiter_depth = 0usize;
    for token in tokens {
        check_string(&token.value)?;
        if let Some(substitution) = &token.subst {
            for segment in &substitution.segments {
                check_string(&segment.text)?;
            }
        }
        match &token.kind {
            TokenKind::LBrace | TokenKind::LBracket => {
                delimiter_depth = delimiter_depth.saturating_add(1);
                if delimiter_depth > MAX_STRUCTURAL_DEPTH + 1 {
                    return Err(ConfigSourceError::StructuralDepth);
                }
            }
            TokenKind::RBrace | TokenKind::RBracket => {
                delimiter_depth = delimiter_depth.saturating_sub(1);
            }
            TokenKind::Comma
            | TokenKind::Colon
            | TokenKind::Equals
            | TokenKind::PlusEquals
            | TokenKind::Newline
            | TokenKind::QuotedString
            | TokenKind::TripleQuotedString
            | TokenKind::Unquoted
            | TokenKind::Substitution
            | TokenKind::Eof => {}
        }
    }
    Ok(())
}

fn check_ast_budget(node: &AstNode) -> Result<(), ConfigSourceError> {
    let mut nodes = 0;
    let mut substitutions = 0;
    check_ast_node(node, 0, &mut nodes, &mut substitutions)
}

fn check_ast_node(
    node: &AstNode,
    depth: usize,
    nodes: &mut usize,
    substitutions: &mut usize,
) -> Result<(), ConfigSourceError> {
    if depth > MAX_STRUCTURAL_DEPTH {
        return Err(ConfigSourceError::StructuralDepth);
    }
    count_node(nodes)?;
    match node {
        AstNode::Object { fields, .. } => {
            for field in fields {
                for key in &field.key {
                    check_string(key)?;
                    count_node(nodes)?;
                }
                check_ast_node(&field.value, depth + 1, nodes, substitutions)?;
            }
        }
        AstNode::Array { items, .. } => {
            for item in items {
                check_ast_node(item, depth + 1, nodes, substitutions)?;
            }
        }
        AstNode::Concat { nodes: items, .. } => {
            for item in items {
                check_ast_node(item, depth, nodes, substitutions)?;
            }
        }
        AstNode::Scalar { value, .. } => check_string(&value.raw)?,
        AstNode::Substitution { segments, .. } => {
            *substitutions = substitutions
                .checked_add(1)
                .ok_or(ConfigSourceError::SubstitutionLimit)?;
            if *substitutions > MAX_SUBSTITUTIONS {
                return Err(ConfigSourceError::SubstitutionLimit);
            }
            for segment in segments {
                check_string(&segment.text)?;
            }
        }
        AstNode::Include { path, .. } => check_string(path)?,
    }
    Ok(())
}

fn count_node(nodes: &mut usize) -> Result<(), ConfigSourceError> {
    *nodes = nodes.checked_add(1).ok_or(ConfigSourceError::NodeLimit)?;
    if *nodes > MAX_NODES {
        return Err(ConfigSourceError::NodeLimit);
    }
    Ok(())
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

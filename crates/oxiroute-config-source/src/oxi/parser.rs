use std::collections::HashSet;

use serde_json::Number;

use crate::{ConfigSourceError, MAX_SOURCE_BYTES, MAX_STRUCTURAL_DEPTH, limits::check_string};

use super::{
    Expr, ObjectBuilder, ParsedDocument, RootItem, Token, bare_to_expr, bump_shared_node,
    collection_for_stanza, is_kv_token, is_list_marker, measure_indent, merge_route_action,
    native_server_spec, parse_err, parse_http_status, token_as_key, token_as_string, tokenize_line,
};

#[derive(Clone, Copy, Eq, PartialEq)]
enum BlockContext {
    Normal,
    Edge,
    Site,
    Template,
    SitesMap,
    TemplatesMap,
}

enum LineAction {
    Insert(String, Expr, u32),
    EdgeRedirect {
        addrs: Expr,
        line: u32,
    },
    PushUse(Vec<Expr>, u32),
    Route {
        tokens: Vec<Token>,
        line: u32,
    },
    NestedBlock {
        key: String,
        ctx: BlockContext,
        line: u32,
    },
}

struct RawLine<'a> {
    number: u32,
    text: &'a str,
}

pub(super) fn parse(source: &str) -> Result<ParsedDocument, ConfigSourceError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(ConfigSourceError::SourceTooLarge);
    }
    let lines: Vec<RawLine<'_>> = source
        .split('\n')
        .enumerate()
        .map(|(index, raw)| RawLine {
            number: u32::try_from(index + 1).unwrap_or(u32::MAX),
            text: raw.strip_suffix('\r').unwrap_or(raw),
        })
        .collect();

    let mut parser = Parser {
        lines: &lines,
        pos: 0,
        nodes: 0,
    };
    Ok(ParsedDocument {
        items: parser.parse_root()?,
    })
}

struct Parser<'a> {
    lines: &'a [RawLine<'a>],
    pos: usize,
    nodes: usize,
}

impl<'a> Parser<'a> {
    fn bump_node(&mut self) -> Result<(), ConfigSourceError> {
        bump_shared_node(&mut self.nodes)
    }

    fn peek_next_active(&self) -> Result<Option<(usize, usize, &'a str, u32)>, ConfigSourceError> {
        let mut idx = self.pos;
        while idx < self.lines.len() {
            let line = &self.lines[idx];
            if let Some((indent, content)) = measure_indent(line.number, line.text)? {
                return Ok(Some((idx, indent, content, line.number)));
            }
            idx += 1;
        }
        Ok(None)
    }

    fn has_indented_child(&self, parent_indent: usize) -> Result<bool, ConfigSourceError> {
        Ok(self
            .peek_next_active()?
            .is_some_and(|(_, indent, _, _)| indent > parent_indent))
    }

    fn reject_children(
        &self,
        parent_indent: usize,
        context: &str,
        line: u32,
    ) -> Result<(), ConfigSourceError> {
        if self.has_indented_child(parent_indent)? {
            let child_line = self.peek_next_active()?.map_or(line, |(_, _, _, ln)| ln);
            return Err(parse_err(
                child_line,
                format!("unexpected indented block after {context}"),
            ));
        }
        Ok(())
    }

    fn parse_root(&mut self) -> Result<Vec<RootItem>, ConfigSourceError> {
        self.bump_node()?;
        let mut items = Vec::new();
        while let Some((idx, indent, content, line_no)) = self.peek_next_active()? {
            self.pos = idx;
            if indent != 0 {
                return Err(parse_err(line_no, "unexpected indentation at top level"));
            }
            if is_list_marker(content) {
                return Err(parse_err(
                    line_no,
                    "top-level list items are not allowed; OXI root must be an object",
                ));
            }
            let tokens = tokenize_line(content, line_no, 0)?;
            self.pos += 1;
            items.push(self.parse_root_item(&tokens, line_no)?);
        }
        Ok(items)
    }

    fn parse_root_item(
        &mut self,
        tokens: &[Token],
        line: u32,
    ) -> Result<RootItem, ConfigSourceError> {
        if let Some(Token::Bare(keyword)) = tokens.first() {
            match keyword.as_str() {
                "include" => {
                    if tokens.len() != 2 {
                        return Err(parse_err(
                            line,
                            "`include` expects exactly one relative file path",
                        ));
                    }
                    self.reject_children(0, "`include` directive", line)?;
                    let path = token_as_string(&tokens[1], line, "`include` path")?;
                    return Ok(RootItem::Include { path, line });
                }
                "lua" if tokens.len() == 1 => {
                    return Ok(RootItem::LuaBlock {
                        code: self.extract_raw_lua_block(0, line)?,
                        line,
                    });
                }
                "site" if tokens.len() >= 2 => {
                    return self.parse_named_map_stanza(tokens, line, true);
                }
                "template" if tokens.len() >= 2 => {
                    return self.parse_named_map_stanza(tokens, line, false);
                }
                "use" if tokens.len() >= 2 => {
                    self.reject_children(0, "inline `use` directive", line)?;
                    let values = self.parse_use_tokens(&tokens[1..], line, 1)?;
                    return Ok(RootItem::Use { values, line });
                }
                kw if tokens.len() >= 2 => {
                    if let Some(collection) = collection_for_stanza(kw) {
                        return self.parse_collection_stanza(collection, tokens, line);
                    }
                    if let Some((key, multi_path)) = native_server_spec(kw) {
                        return self.parse_native_server_stanza(key, multi_path, tokens, line);
                    }
                }
                _ => {}
            }
        }

        let child_ctx = match tokens.first() {
            Some(Token::Bare(kw)) if kw == "edge" => BlockContext::Edge,
            Some(Token::Bare(kw)) if kw == "sites" => BlockContext::SitesMap,
            Some(Token::Bare(kw)) if kw == "templates" => BlockContext::TemplatesMap,
            _ => BlockContext::Normal,
        };
        let key = token_as_key(&tokens[0], line)?;
        let value = match self.classify_entry(&key, tokens, 0, line, 0, child_ctx)? {
            LineAction::Insert(_, val, _) => val,
            LineAction::NestedBlock { ctx, .. } => self.parse_block(0, 1, ctx)?,
            _ => unreachable!(),
        };
        Ok(RootItem::Entry { key, value, line })
    }

    fn parse_named_map_stanza(
        &mut self,
        tokens: &[Token],
        line: u32,
        is_site: bool,
    ) -> Result<RootItem, ConfigSourceError> {
        let label = if is_site { "site" } else { "template" };
        if tokens.len() != 2 {
            return Err(parse_err(
                line,
                format!("`{label}` stanza expects exactly one name"),
            ));
        }
        let name = token_as_key(&tokens[1], line)?;
        let ctx = if is_site {
            BlockContext::Site
        } else {
            BlockContext::Template
        };
        let body = self.parse_optional_object_body(0, 2, ctx)?;
        Ok(if is_site {
            RootItem::Site { name, body, line }
        } else {
            RootItem::Template { name, body, line }
        })
    }

    fn parse_collection_stanza(
        &mut self,
        collection: &'static str,
        tokens: &[Token],
        line: u32,
    ) -> Result<RootItem, ConfigSourceError> {
        if tokens.len() != 2 {
            return Err(parse_err(line, "named stanza expects exactly one name"));
        }
        let name = token_as_key(&tokens[1], line)?;
        let body = self.parse_optional_object_body(0, 2, BlockContext::Normal)?;
        if let Expr::Object(entries) = &body
            && let Some((_, _, dup_line)) = entries.iter().find(|(k, _, _)| k == "name")
        {
            return Err(parse_err(*dup_line, "duplicate key `name` in named stanza"));
        }
        Ok(RootItem::NamedCollection {
            collection,
            name,
            body,
            line,
        })
    }

    fn parse_native_server_stanza(
        &mut self,
        key: &'static str,
        multi_path: bool,
        tokens: &[Token],
        line: u32,
    ) -> Result<RootItem, ConfigSourceError> {
        let mut entries = Vec::new();
        let path_field = if multi_path {
            let mut paths = Vec::with_capacity(tokens.len() - 1);
            for tok in &tokens[1..] {
                self.bump_node()?;
                paths.push(Expr::String(token_as_string(tok, line, key)?));
            }
            entries.push(("paths".to_owned(), Expr::Array(paths), line));
            "paths"
        } else {
            if tokens.len() != 2 {
                return Err(parse_err(
                    line,
                    format!("`{key}` expects exactly one path argument"),
                ));
            }
            self.bump_node()?;
            let path = token_as_string(&tokens[1], line, key)?;
            entries.push(("path".to_owned(), Expr::String(path), line));
            "path"
        };

        if let Expr::Object(body_entries) =
            self.parse_optional_object_body(0, 2, BlockContext::Normal)?
        {
            for (field, value, field_line) in body_entries {
                if field == path_field {
                    return Err(parse_err(
                        field_line,
                        format!("duplicate key `{path_field}` in `{key}` stanza"),
                    ));
                }
                entries.push((field, value, field_line));
            }
        }
        self.bump_node()?;
        Ok(RootItem::NativeServer {
            key,
            value: Expr::Object(entries),
            line,
        })
    }

    fn parse_optional_object_body(
        &mut self,
        parent_indent: usize,
        depth: usize,
        ctx: BlockContext,
    ) -> Result<Expr, ConfigSourceError> {
        if let Some((_, child_indent, content, line_no)) = self.peek_next_active()?
            && child_indent > parent_indent
        {
            if is_list_marker(content) {
                return Err(parse_err(
                    line_no,
                    "expected key-value entries in stanza body, found list item",
                ));
            }
            return self.parse_object_body(child_indent, depth, ctx, None);
        }
        self.bump_node()?;
        Ok(Expr::Object(Vec::new()))
    }

    fn parse_block(
        &mut self,
        parent_indent: usize,
        depth: usize,
        ctx: BlockContext,
    ) -> Result<Expr, ConfigSourceError> {
        if depth > MAX_STRUCTURAL_DEPTH {
            return Err(ConfigSourceError::StructuralDepth);
        }
        let Some((idx, child_indent, content, line_no)) = self.peek_next_active()? else {
            return Err(ConfigSourceError::parse("OXI", "expected indented block"));
        };
        if child_indent <= parent_indent {
            return Err(parse_err(line_no, "expected indented block"));
        }
        if is_list_marker(content) {
            return self.parse_list_block(child_indent, depth);
        }
        if content.starts_with("lua") {
            let first_tokens = tokenize_line(content, line_no, depth)?;
            if first_tokens.len() == 1 && matches!(&first_tokens[0], Token::Bare(kw) if kw == "lua")
            {
                self.pos = idx + 1;
                let code = self.extract_raw_lua_block(child_indent, line_no)?;
                if let Some((_, next_indent, _, next_line)) = self.peek_next_active()?
                    && next_indent == child_indent
                {
                    return Err(parse_err(
                        next_line,
                        "unexpected sibling entry after `lua` block",
                    ));
                }
                self.bump_node()?;
                return Ok(Expr::LuaBlock(code));
            }
        }
        self.parse_object_body(child_indent, depth, ctx, None)
    }

    fn parse_object_body(
        &mut self,
        block_indent: usize,
        depth: usize,
        ctx: BlockContext,
        first_action: Option<LineAction>,
    ) -> Result<Expr, ConfigSourceError> {
        if depth > MAX_STRUCTURAL_DEPTH {
            return Err(ConfigSourceError::StructuralDepth);
        }
        self.bump_node()?;
        let mut state = Box::new(ObjectBuilder::default());
        let mut pending = first_action;

        loop {
            let action = if let Some(action) = pending.take() {
                action
            } else {
                let Some((idx, indent, content, line_no)) = self.peek_next_active()? else {
                    break;
                };
                if indent < block_indent {
                    break;
                }
                if indent > block_indent {
                    return Err(parse_err(line_no, "unexpected indentation in object block"));
                }
                if is_list_marker(content) {
                    return Err(parse_err(
                        line_no,
                        "unexpected list item inside object block",
                    ));
                }
                self.pos = idx + 1;
                self.classify_object_line(content, block_indent, line_no, depth, ctx)?
            };
            match action {
                LineAction::Insert(key, val, line) => state.insert(key, val, line)?,
                LineAction::EdgeRedirect { addrs, line } => {
                    state.insert("http".to_owned(), addrs, line)?;
                    self.bump_node()?;
                    state.insert("redirect_https".to_owned(), Expr::Bool(true), line)?;
                }
                LineAction::PushUse(vals, line) => state.push_use(vals, line),
                LineAction::Route { tokens, line } => {
                    let route =
                        self.parse_route_shorthand(&tokens, block_indent, line, depth + 2)?;
                    state.push_route(route, line);
                }
                LineAction::NestedBlock {
                    key,
                    ctx: child_ctx,
                    line,
                } => {
                    let val = self.parse_block(block_indent, depth + 1, child_ctx)?;
                    state.insert(key, val, line)?;
                }
            }
        }
        state.finish(&mut self.nodes)
    }

    #[inline(never)]
    fn classify_object_line(
        &mut self,
        content: &str,
        indent: usize,
        line: u32,
        depth: usize,
        ctx: BlockContext,
    ) -> Result<LineAction, ConfigSourceError> {
        let tokens = tokenize_line(content, line, depth)?;
        self.classify_tokens(tokens, indent, line, depth, ctx)
    }

    fn classify_tokens(
        &mut self,
        tokens: Vec<Token>,
        indent: usize,
        line: u32,
        depth: usize,
        ctx: BlockContext,
    ) -> Result<LineAction, ConfigSourceError> {
        if let Some(Token::Bare(kw)) = tokens.first() {
            if ctx == BlockContext::Edge
                && kw == "http"
                && tokens.len() >= 4
                && matches!(&tokens[tokens.len() - 2], Token::Bare(a) if a == "->")
                && matches!(&tokens[tokens.len() - 1], Token::Bare(h) if h == "https")
            {
                self.reject_children(indent, "`http ... -> https` directive", line)?;
                self.bump_node()?;
                let mut addrs = Vec::with_capacity(tokens.len() - 3);
                for tok in &tokens[1..tokens.len() - 2] {
                    addrs.push(self.token_to_expr(tok, line, depth + 2)?);
                }
                return Ok(LineAction::EdgeRedirect {
                    addrs: Expr::Array(addrs),
                    line,
                });
            }
            if ctx == BlockContext::Template && kw == "params" && tokens.len() >= 2 {
                self.reject_children(indent, "`params` directive", line)?;
                let obj = self.parse_kv_object(&tokens[1..], line, depth + 1, None)?;
                return Ok(LineAction::Insert("params".to_owned(), obj, line));
            }
            if kw == "use" && tokens.len() >= 2 {
                self.reject_children(indent, "`use` directive", line)?;
                let vals = self.parse_use_tokens(&tokens[1..], line, depth + 1)?;
                return Ok(LineAction::PushUse(vals, line));
            }
            if ctx == BlockContext::Site && kw == "route" {
                return Ok(LineAction::Route { tokens, line });
            }
        }

        let key = token_as_key(&tokens[0], line)?;
        let child_ctx = match ctx {
            BlockContext::SitesMap => BlockContext::Site,
            BlockContext::TemplatesMap => BlockContext::Template,
            _ => BlockContext::Normal,
        };
        self.classify_entry(&key, &tokens, indent, line, depth, child_ctx)
    }

    fn classify_entry(
        &mut self,
        key: &str,
        tokens: &[Token],
        key_indent: usize,
        line: u32,
        depth: usize,
        child_ctx: BlockContext,
    ) -> Result<LineAction, ConfigSourceError> {
        if depth + 1 > MAX_STRUCTURAL_DEPTH {
            return Err(ConfigSourceError::StructuralDepth);
        }
        if tokens.len() == 3
            && matches!(&tokens[1], Token::Bare(eq) if eq == "=")
            && matches!(&tokens[2], Token::Bare(kw) if kw == "lua")
        {
            let code = self.extract_raw_lua_block(key_indent, line)?;
            self.bump_node()?;
            return Ok(LineAction::Insert(
                key.to_owned(),
                Expr::LuaBlock(code),
                line,
            ));
        }
        if tokens.len() == 1 {
            if !self.has_indented_child(key_indent)? {
                return Err(parse_err(
                    line,
                    format!("expected value or indented block after key `{key}`"),
                ));
            }
            return Ok(LineAction::NestedBlock {
                key: key.to_owned(),
                ctx: child_ctx,
                line,
            });
        }
        self.reject_children(key_indent, "inline value", line)?;
        let val = if tokens.len() == 2 {
            self.token_to_expr(&tokens[1], line, depth + 1)?
        } else {
            self.bump_node()?;
            let mut items = Vec::with_capacity(tokens.len() - 1);
            for tok in &tokens[1..] {
                items.push(self.token_to_expr(tok, line, depth + 2)?);
            }
            Expr::Array(items)
        };
        Ok(LineAction::Insert(key.to_owned(), val, line))
    }

    fn parse_route_shorthand(
        &mut self,
        tokens: &[Token],
        indent: usize,
        line: u32,
        depth: usize,
    ) -> Result<Expr, ConfigSourceError> {
        if depth > MAX_STRUCTURAL_DEPTH {
            return Err(ConfigSourceError::StructuralDepth);
        }
        if tokens.len() < 4 || !matches!(&tokens[2], Token::Bare(arrow) if arrow == "->") {
            return Err(parse_err(
                line,
                "`route` expects `route <path> -> <target...>`",
            ));
        }
        self.bump_node()?;
        let path_expr = Expr::String(token_as_string(&tokens[1], line, "`route` path")?);
        let (action_key, mut action_val) =
            self.parse_route_target(&tokens[3..], line, depth + 1)?;

        let mut route_entries = vec![("path".to_owned(), path_expr, line)];
        let mut seen = HashSet::from(["path".to_owned()]);

        if let Expr::Object(body_entries) =
            self.parse_optional_object_body(indent, depth + 1, BlockContext::Normal)?
        {
            for (field, field_val, field_line) in body_entries {
                if field == action_key {
                    action_val = merge_route_action(action_key, action_val, field_val, field_line)?;
                    continue;
                }
                if !seen.insert(field.clone()) {
                    return Err(parse_err(
                        field_line,
                        format!("duplicate key `{field}` in `route` block"),
                    ));
                }
                route_entries.push((field, field_val, field_line));
            }
        }
        route_entries.push((action_key.to_owned(), action_val, line));
        Ok(Expr::Object(route_entries))
    }

    fn parse_route_target(
        &mut self,
        tokens: &[Token],
        line: u32,
        depth: usize,
    ) -> Result<(&'static str, Expr), ConfigSourceError> {
        if let Some(Token::Bare(first)) = tokens.first() {
            if let Some(status) = parse_http_status(first) {
                return self.build_respond_action(status, &tokens[1..], line, depth);
            }
            match first.as_str() {
                "respond" => {
                    let Some(Token::Bare(code_tok)) = tokens.get(1) else {
                        return Err(parse_err(
                            line,
                            "`route ... -> respond` expects an HTTP status code (100-599)",
                        ));
                    };
                    let status = parse_http_status(code_tok).ok_or_else(|| {
                        parse_err(line, format!("invalid HTTP status code `{code_tok}`"))
                    })?;
                    return self.build_respond_action(status, &tokens[2..], line, depth);
                }
                "static" => {
                    if tokens.len() != 2 {
                        return Err(parse_err(
                            line,
                            "`route ... -> static` expects exactly one root path",
                        ));
                    }
                    return Ok(("static", self.token_to_expr(&tokens[1], line, depth)?));
                }
                "redirect" => return self.build_redirect_action(&tokens[1..], line, depth),
                "proxy" => {
                    if tokens.len() < 2 {
                        return Err(parse_err(
                            line,
                            "`route ... -> proxy` expects at least one target",
                        ));
                    }
                    return self.build_proxy_action(&tokens[1..], line, depth);
                }
                _ => {}
            }
        }
        self.build_proxy_action(tokens, line, depth)
    }

    fn build_respond_action(
        &mut self,
        status: u16,
        body_tokens: &[Token],
        line: u32,
        depth: usize,
    ) -> Result<(&'static str, Expr), ConfigSourceError> {
        self.bump_node()?;
        let mut entries = vec![(
            "status".to_owned(),
            Expr::Number(Number::from(status)),
            line,
        )];
        if body_tokens.len() == 1 {
            let body = match &body_tokens[0] {
                Token::Bare(s) | Token::Quoted(s) => {
                    self.bump_node()?;
                    Expr::String(s.clone())
                }
                other => self.token_to_expr(other, line, depth + 1)?,
            };
            entries.push(("body".to_owned(), body, line));
        } else if body_tokens.len() > 1 {
            let mut joined = String::new();
            for (idx, tok) in body_tokens.iter().enumerate() {
                if idx > 0 {
                    joined.push(' ');
                }
                joined.push_str(&token_as_string(tok, line, "`route` response body")?);
            }
            check_string(&joined)?;
            self.bump_node()?;
            entries.push(("body".to_owned(), Expr::String(joined), line));
        }
        Ok(("respond", Expr::Object(entries)))
    }

    fn build_redirect_action(
        &mut self,
        args: &[Token],
        line: u32,
        depth: usize,
    ) -> Result<(&'static str, Expr), ConfigSourceError> {
        self.bump_node()?;
        let (status, target_tok) = match args {
            [target] => (308_u16, target),
            [Token::Bare(code), target] => {
                let status = parse_http_status(code).ok_or_else(|| {
                    parse_err(line, format!("invalid HTTP redirect status `{code}`"))
                })?;
                (status, target)
            }
            _ => {
                return Err(parse_err(
                    line,
                    "`route ... -> redirect` expects `[status] <url>`",
                ));
            }
        };
        let to = self.token_to_expr(target_tok, line, depth + 1)?;
        Ok((
            "redirect",
            Expr::Object(vec![
                (
                    "status".to_owned(),
                    Expr::Number(Number::from(status)),
                    line,
                ),
                ("to".to_owned(), to, line),
            ]),
        ))
    }

    fn build_proxy_action(
        &mut self,
        args: &[Token],
        line: u32,
        depth: usize,
    ) -> Result<(&'static str, Expr), ConfigSourceError> {
        if args.len() == 1 {
            return Ok(("proxy", self.token_to_expr(&args[0], line, depth)?));
        }
        self.bump_node()?;
        let mut list = Vec::with_capacity(args.len());
        for tok in args {
            list.push(self.token_to_expr(tok, line, depth + 1)?);
        }
        Ok(("proxy", Expr::Array(list)))
    }

    fn parse_use_tokens(
        &mut self,
        tokens: &[Token],
        line: u32,
        depth: usize,
    ) -> Result<Vec<Expr>, ConfigSourceError> {
        if !tokens.iter().any(is_kv_token) {
            let mut names = Vec::with_capacity(tokens.len());
            for tok in tokens {
                self.bump_node()?;
                names.push(Expr::String(token_as_string(
                    tok,
                    line,
                    "`use` template name",
                )?));
            }
            return Ok(names);
        }
        if is_kv_token(&tokens[0]) {
            return Err(parse_err(
                line,
                "parameterized `use` must start with a template name before `key=value` pairs",
            ));
        }
        let name = token_as_string(&tokens[0], line, "`use` template name")?;
        Ok(vec![self.parse_kv_object(
            &tokens[1..],
            line,
            depth,
            Some(name),
        )?])
    }

    fn parse_kv_object(
        &mut self,
        tokens: &[Token],
        line: u32,
        depth: usize,
        initial_name: Option<String>,
    ) -> Result<Expr, ConfigSourceError> {
        self.bump_node()?;
        let mut entries = Vec::with_capacity(tokens.len() + usize::from(initial_name.is_some()));
        let mut seen = HashSet::new();
        if let Some(name) = initial_name {
            seen.insert("name".to_owned());
            entries.push(("name".to_owned(), Expr::String(name), line));
        }
        for tok in tokens {
            let (key, val_expr) = match tok {
                Token::Kv(k, v) => {
                    check_string(k)?;
                    (k.clone(), self.token_to_expr(v, line, depth + 1)?)
                }
                Token::Bare(raw) => {
                    let Some((k, v)) = raw.split_once('=') else {
                        return Err(parse_err(
                            line,
                            format!("expected `key=value`, found `{raw}`"),
                        ));
                    };
                    if k.is_empty() {
                        return Err(parse_err(
                            line,
                            format!("parameter key must not be empty in `{raw}`"),
                        ));
                    }
                    check_string(k)?;
                    self.bump_node()?;
                    (k.to_owned(), bare_to_expr(v)?)
                }
                _ => return Err(parse_err(line, "expected `key=value` pair")),
            };
            if !seen.insert(key.clone()) {
                return Err(parse_err(line, format!("duplicate parameter `{key}`")));
            }
            entries.push((key, val_expr, line));
        }
        Ok(Expr::Object(entries))
    }

    fn parse_list_block(
        &mut self,
        dash_indent: usize,
        depth: usize,
    ) -> Result<Expr, ConfigSourceError> {
        if depth > MAX_STRUCTURAL_DEPTH {
            return Err(ConfigSourceError::StructuralDepth);
        }
        self.bump_node()?;
        let mut items = Vec::new();
        while let Some((idx, indent, content, line_no)) = self.peek_next_active()? {
            if indent < dash_indent {
                break;
            }
            if indent > dash_indent {
                return Err(parse_err(line_no, "unexpected indentation in list"));
            }
            if !is_list_marker(content) {
                return Err(parse_err(line_no, "expected `-` list item"));
            }
            self.pos = idx + 1;
            items.push(self.parse_list_item(content, dash_indent, line_no, depth)?);
        }
        Ok(Expr::Array(items))
    }

    fn parse_list_item(
        &mut self,
        content: &str,
        dash_indent: usize,
        line_no: u32,
        depth: usize,
    ) -> Result<Expr, ConfigSourceError> {
        let after_dash = &content[1..];
        let trimmed = after_dash.trim_start_matches(' ');
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return self.parse_block(dash_indent, depth + 1, BlockContext::Normal);
        }

        let content_col = dash_indent + 1 + (after_dash.len() - trimmed.len());
        let tokens = tokenize_line(trimmed, line_no, depth + 1)?;
        let has_children = self.has_indented_child(dash_indent)?;

        if !has_children && tokens.len() == 1 {
            return self.token_to_expr(&tokens[0], line_no, depth + 1);
        }
        if has_children && tokens.len() == 1 && matches!(&tokens[0], Token::Bare(kw) if kw == "lua")
        {
            let code = self.extract_raw_lua_block(dash_indent, line_no)?;
            self.bump_node()?;
            return Ok(Expr::LuaBlock(code));
        }

        let first = self.classify_tokens(
            tokens,
            content_col,
            line_no,
            depth + 1,
            BlockContext::Normal,
        )?;
        let obj =
            self.parse_object_body(content_col, depth + 1, BlockContext::Normal, Some(first))?;
        if let Some((_, next_indent, _, next_line)) = self.peek_next_active()?
            && next_indent > dash_indent
        {
            return Err(parse_err(next_line, "unexpected indentation in list item"));
        }
        Ok(obj)
    }

    fn extract_raw_lua_block(
        &mut self,
        parent_indent: usize,
        header_line: u32,
    ) -> Result<String, ConfigSourceError> {
        let mut block_lines: Vec<Option<&str>> = Vec::new();
        let mut min_indent: Option<usize> = None;

        while self.pos < self.lines.len() {
            let raw = self.lines[self.pos].text;
            if raw.trim().is_empty() {
                block_lines.push(None);
                self.pos += 1;
                continue;
            }
            let leading = raw.bytes().take_while(|&b| b == b' ').count();
            if leading <= parent_indent {
                break;
            }
            min_indent = Some(min_indent.map_or(leading, |cur| cur.min(leading)));
            block_lines.push(Some(raw));
            self.pos += 1;
        }
        while matches!(block_lines.last(), Some(None)) {
            block_lines.pop();
        }
        let Some(strip) = min_indent else {
            return Err(parse_err(
                header_line,
                "expected indented Lua code block after `lua`",
            ));
        };

        let mut code = String::new();
        for (idx, entry) in block_lines.into_iter().enumerate() {
            if idx > 0 {
                code.push('\n');
            }
            if let Some(raw) = entry {
                code.push_str(&raw[strip..]);
            }
        }
        if code.len() > MAX_SOURCE_BYTES {
            return Err(ConfigSourceError::SourceTooLarge);
        }
        Ok(code)
    }

    fn token_to_expr(
        &mut self,
        token: &Token,
        line: u32,
        depth: usize,
    ) -> Result<Expr, ConfigSourceError> {
        if depth > MAX_STRUCTURAL_DEPTH {
            return Err(ConfigSourceError::StructuralDepth);
        }
        self.bump_node()?;
        match token {
            Token::Bare(s) => bare_to_expr(s),
            Token::Quoted(s) => {
                check_string(s)?;
                Ok(Expr::String(s.clone()))
            }
            Token::EmptyObject => Ok(Expr::Object(Vec::new())),
            Token::InlineArray(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.token_to_expr(item, line, depth + 1)?);
                }
                Ok(Expr::Array(out))
            }
            Token::LuaExpr(code) => Ok(Expr::LuaInline(code.clone())),
            Token::Kv(k, _) => Err(parse_err(
                line,
                format!("unexpected `key=value` token `{k}=...` outside `params` or `use`"),
            )),
        }
    }
}

impl Renderer {
    fn table_list_field<T>(&mut self, name: &str, values: &[T], render: fn(&mut Self, &T)) {
        self.begin_table_field(name);
        for value in values {
            self.begin_table_item();
            render(self, value);
            self.end_table();
        }
        self.end_table();
    }

    fn table_list_or_nil_field<T>(&mut self, name: &str, values: &[T], render: fn(&mut Self, &T)) {
        if values.is_empty() {
            self.nil_field(name);
        } else {
            self.table_list_field(name, values, render);
        }
    }

    fn fallible_table_list_field<T, F>(
        &mut self,
        name: &str,
        values: &[T],
        mut render: F,
    ) -> Result<(), ConfigError>
    where
        F: FnMut(&mut Self, &T) -> Result<(), ConfigError>,
    {
        self.begin_table_field(name);
        for value in values {
            self.begin_table_item();
            render(self, value)?;
            self.end_table();
        }
        self.end_table();
        Ok(())
    }

    fn optional_table_field<T>(
        &mut self,
        name: &str,
        value: Option<&T>,
        render: fn(&mut Self, &T),
    ) {
        match value {
            Some(value) => {
                self.begin_table_field(name);
                render(self, value);
                self.end_table();
            }
            None => self.nil_field(name),
        }
    }

    fn fallible_optional_table_field<T>(
        &mut self,
        name: &str,
        value: Option<&T>,
        render: fn(&mut Self, &T) -> Result<(), ConfigError>,
    ) -> Result<(), ConfigError> {
        match value {
            Some(value) => {
                self.begin_table_field(name);
                render(self, value)?;
                self.end_table();
            }
            None => self.nil_field(name),
        }
        Ok(())
    }

    fn begin_table_field(&mut self, name: &str) {
        self.indent();
        push_lua_field_name(&mut self.output, name);
        self.output.push_str(" = {\n");
        self.indent += 1;
    }

    fn begin_table_item(&mut self) {
        self.indent();
        self.output.push_str("{\n");
        self.indent += 1;
    }

    fn end_table(&mut self) {
        self.indent -= 1;
        self.indent();
        self.output.push_str("},\n");
    }

    fn string_field(&mut self, name: &str, value: &str) {
        self.field_name(name);
        push_lua_string(&mut self.output, value);
        self.output.push_str(",\n");
    }

    fn optional_string_field(&mut self, name: &str, value: Option<&str>) {
        match value {
            Some(value) => self.string_field(name, value),
            None => self.nil_field(name),
        }
    }

    fn optional_integer_field<T: Display>(&mut self, name: &str, value: Option<T>) {
        match value {
            Some(value) => self.integer_field(name, value),
            None => self.nil_field(name),
        }
    }

    fn optional_boolean_field(&mut self, name: &str, value: Option<bool>) {
        match value {
            Some(value) => self.boolean_field(name, value),
            None => self.nil_field(name),
        }
    }

    fn access_log_field(
        &mut self,
        field: &str,
        policy: Option<&AccessLogPolicy>,
        kind: &'static str,
        name: &str,
    ) -> Result<(), ConfigError> {
        match policy {
            Some(AccessLogPolicy::Disabled) => {
                self.begin_table_field(field);
                self.string_field("type", "disabled");
                self.end_table();
            }
            Some(AccessLogPolicy::File { path }) => {
                self.begin_table_field(field);
                self.string_field("type", "file");
                self.string_field("path", utf8_path(path, kind, name, "access_log.path")?);
                self.end_table();
            }
            None => self.nil_field(field),
        }
        Ok(())
    }

    fn string_list_field<S>(&mut self, name: &str, values: &[S])
    where
        S: AsRef<str>,
    {
        self.field_name(name);
        self.output.push('{');
        for (index, value) in values.iter().enumerate() {
            if index == 0 {
                self.output.push(' ');
            } else {
                self.output.push_str(", ");
            }
            push_lua_string(&mut self.output, value.as_ref());
        }
        if !values.is_empty() {
            self.output.push(' ');
        }
        self.output.push_str("},\n");
    }

    fn integer_list_field<T: Display>(&mut self, name: &str, values: &[T]) {
        self.field_name(name);
        self.output.push('{');
        for (index, value) in values.iter().enumerate() {
            if index == 0 {
                self.output.push(' ');
            } else {
                self.output.push_str(", ");
            }
            write!(self.output, "{value}").expect("writing to String cannot fail");
        }
        if !values.is_empty() {
            self.output.push(' ');
        }
        self.output.push_str("},\n");
    }

    fn integer_field(&mut self, name: &str, value: impl Display) {
        self.field_name(name);
        write!(self.output, "{value}").expect("writing to String cannot fail");
        self.output.push_str(",\n");
    }

    fn boolean_field(&mut self, name: &str, value: bool) {
        self.field_name(name);
        self.output.push_str(if value { "true" } else { "false" });
        self.output.push_str(",\n");
    }

    fn nil_field(&mut self, name: &str) {
        self.field_name(name);
        self.output.push_str("nil,\n");
    }

    fn null_field(&mut self, name: &str) {
        self.field_name(name);
        self.output.push_str("null,\n");
    }

    fn field_name(&mut self, name: &str) {
        self.indent();
        push_lua_field_name(&mut self.output, name);
        self.output.push_str(" = ");
    }

    fn indent(&mut self) {
        for _ in 0..self.indent {
            self.output.push_str("  ");
        }
    }
}

fn push_lua_field_name(output: &mut String, name: &str) {
    const KEYWORDS: &[&str] = &[
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if",
        "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ];
    let mut characters = name.chars();
    let identifier = characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric());
    if identifier && !KEYWORDS.contains(&name) {
        output.push_str(name);
    } else {
        output.push('[');
        push_lua_string(output, name);
        output.push(']');
    }
}

fn utf8_path<'a>(
    path: &'a Path,
    kind: &'static str,
    name: &str,
    field: &'static str,
) -> Result<&'a str, ConfigError> {
    path.to_str().ok_or_else(|| ConfigError::InvalidFilePath {
        kind,
        name: name.into(),
        field,
        detail: "path must be valid UTF-8",
    })
}

const fn forward_access_action(action: ForwardAccessAction) -> &'static str {
    match action {
        ForwardAccessAction::Allow => "allow",
        ForwardAccessAction::Deny => "deny",
    }
}

fn utf8_recording_root<'a>(
    path: &'a Path,
    service: &str,
    application: &str,
    recorder: &str,
) -> Result<&'a str, ConfigError> {
    path.to_str()
        .ok_or_else(|| ConfigError::InvalidRtmpRecorderPolicy {
            service: service.into(),
            application: application.into(),
            recorder: recorder.into(),
            field: "root_directory",
            detail: "path must be valid UTF-8",
        })
}

fn utf8_http_route_path<'a>(
    path: &'a Path,
    service: &str,
    route: usize,
    field: &'static str,
) -> Result<&'a str, ConfigError> {
    path.to_str().ok_or_else(|| ConfigError::InvalidHttpRoute {
        service: service.into(),
        route,
        field,
        detail: "path must be valid UTF-8".into(),
    })
}

fn http_version(version: HttpVersion) -> &'static str {
    match version {
        HttpVersion::Http11 => "1.1",
        HttpVersion::Http2 => "2",
        HttpVersion::Http3 => "3",
    }
}

fn push_lua_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                write!(output, "\\u{{{:x}}}", u32::from(character))
                    .expect("writing to String cannot fail");
            }
            character => output.push(character),
        }
    }
    output.push('"');
}

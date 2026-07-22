use oxiroute_rtmp::{DirectiveError, NginxParseError, parse_nginx_config};

#[test]
fn parses_nested_rtmp_and_http_configuration() {
    let config = parse_nginx_config(
        r#"
rtmp_auto_push on;
rtmp {
  server {
    listen 1935 proxy_protocol;
    chunk_size 4096;

    application live {
      live on;
      meta copy;
      record audio video;
      push rtmp://backup.example/live app=archive;

      recorder archive {
        record all manual;
        record_path "/var/media files";
      }
    }
  }
}

http {
  server {
    listen 8080;
    location /stat {
      rtmp_stat all;
      rtmp_stat_stylesheet /stat.xsl;
    }
  }
}
"#,
    )
    .expect("valid nginx-rtmp config");

    assert_eq!(config.len(), 3);
    assert_eq!(config[0].name, "rtmp_auto_push");
    let server = child(&config[1], "server");
    let application = child(server, "application");
    assert_eq!(application.args, ["live"]);
    assert_eq!(child(application, "recorder").args, ["archive"]);
    assert_eq!(
        child(child(child(&config[2], "server"), "location"), "rtmp_stat").args,
        ["all"]
    );
}

#[test]
fn handles_comments_quoted_tokens_and_escapes() {
    let config = parse_nginx_config(
        r#"
rtmp {
  server {
    application live {
      # The escaped space remains in one nginx token.
      record_path /var/media\ files;
      log_format stream '$remote_addr [$time_local] "$app/$name"';
    }
  }
}
"#,
    )
    .expect("valid quoted config");

    let application = child(child(&config[0], "server"), "application");
    assert_eq!(child(application, "record_path").args, ["/var/media files"]);
    assert_eq!(
        child(application, "log_format").args,
        ["stream", "$remote_addr [$time_local] \"$app/$name\""]
    );
}

#[test]
fn rejects_invalid_context_values_and_unknown_rtmp_keys() {
    let invalid_context = parse_nginx_config("hls on;").expect_err("hls is not top-level");
    assert!(matches!(
        invalid_context,
        NginxParseError::Directive(DirectiveError::InvalidContext { .. })
    ));

    let invalid_value =
        parse_nginx_config("rtmp { server { application live { hls_fragment_naming random; } } }")
            .expect_err("closed enum");
    assert!(matches!(
        invalid_value,
        NginxParseError::Directive(DirectiveError::InvalidValue { .. })
    ));

    let unknown =
        parse_nginx_config("rtmp { server { mystery on; } }").expect_err("unknown RTMP directive");
    assert!(matches!(
        unknown,
        NginxParseError::UnknownRtmpDirective { .. }
    ));
}

#[test]
fn enforces_block_and_statement_shapes() {
    assert!(matches!(
        parse_nginx_config("rtmp;").expect_err("rtmp requires a block"),
        NginxParseError::InvalidBlockShape { .. }
    ));
    assert!(matches!(
        parse_nginx_config("rtmp { server { live { } } }").expect_err("live is a statement"),
        NginxParseError::InvalidBlockShape { .. }
    ));
}

fn child<'a>(
    parent: &'a oxiroute_rtmp::NginxDirective,
    name: &str,
) -> &'a oxiroute_rtmp::NginxDirective {
    parent
        .children
        .as_ref()
        .expect("block")
        .iter()
        .find(|directive| directive.name == name)
        .expect("named child")
}

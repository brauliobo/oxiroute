use oxiroute_config_source::{ConfigFormat, MAX_STRING_BYTES, decode_value};
use serde_json::{Value, json};

fn eval_lua(script: &str) -> Value {
    decode_value(ConfigFormat::Lua, script.as_bytes())
        .unwrap_or_else(|error| panic!("expected Lua script to succeed, got error: {error}"))
}

fn eval_lua_err(script: &str) -> String {
    decode_value(ConfigFormat::Lua, script.as_bytes())
        .expect_err("expected Lua script to fail")
        .to_string()
}

#[test]
fn object_and_empty_array_preservation() {
    let val = eval_lua(
        r"
        return {
            explicit_obj = object({}),
            converted_obj = object(array({})),
            empty_map = map({}, function(x) return x end),
            empty_filter = filter({ 1, 2, 3 }, function(x) return x > 10 end),
            empty_flat_map = flat_map({ 1, 2 }, function(_) return array({}) end),
            empty_keys = keys({}),
            empty_array_keys = keys(array({})),
            empty_values = values({}),
            empty_array_values = values(array({})),
            empty_concat = concat(),
            empty_concat_lists = concat(array({}), {}),
            empty_unique = unique({}),
            empty_slice = slice({ 1, 2, 3 }, 3, 1),
            empty_range = range(5, 1),
            empty_split = str.split('', ''),
        }
        ",
    );

    assert_eq!(
        val,
        json!({
            "explicit_obj": {},
            "converted_obj": {},
            "empty_map": [],
            "empty_filter": [],
            "empty_flat_map": [],
            "empty_keys": [],
            "empty_array_keys": [],
            "empty_values": [],
            "empty_array_values": [],
            "empty_concat": [],
            "empty_concat_lists": [],
            "empty_unique": [],
            "empty_slice": [],
            "empty_range": [],
            "empty_split": [],
        })
    );

    assert!(eval_lua_err("return object(42)").contains("table"));
    assert!(eval_lua_err("return object('nope')").contains("table"));
}

#[test]
fn clone_deep_copies_preserves_array_mt_allows_dags_and_rejects_cycles() {
    let val = eval_lua(
        r"
        local shared = { count = 1, tags = { 'a', 'b' }, empty = array({}) }
        local orig = { x = shared, y = shared, n = null }
        local copy = clone(orig)
        copy.x.count = 99
        copy.x.tags[1] = 'z'
        return {
            orig_x = orig.x.count,
            orig_tag = orig.x.tags[1],
            copy_x = copy.x.count,
            copy_y = copy.y.count,
            copy_tag = copy.x.tags[1],
            copy_empty = copy.x.empty,
            copy_null = copy.n,
            scalar = clone('hello'),
        }
        ",
    );

    assert_eq!(
        val,
        json!({
            "orig_x": 1,
            "orig_tag": "a",
            "copy_x": 99,
            "copy_y": 1,
            "copy_tag": "z",
            "copy_empty": [],
            "copy_null": null,
            "scalar": "hello",
        })
    );

    let err = eval_lua_err(
        r"
        local t = {}
        t.self = t
        return clone(t)
        ",
    );
    assert!(err.contains("cyclic"), "unexpected error: {err}");
}

#[test]
fn merge_and_deep_merge_semantics() {
    let val = eval_lua(
        r"
        local a = { host = 'a', tls = { min = '1.2', alpn = { 'h2' } }, list = { 1, 2 } }
        local b = { host = 'b', tls = { max = '1.3', alpn = { 'http/1.1' } }, list = { 3 } }
        local c = { extra = true, tls = { sni = 'example.com' } }

        local shallow = merge(a, b)
        local deep = deep_merge(a, b, c)
        local replace_with_null = deep_merge({ nested = { k = 1 } }, { nested = null })
        local replace_with_empty_array = deep_merge({ nested = { k = 1 } }, { nested = array({}) })
        local merge_with_empty_object = deep_merge({ nested = { k = 1 } }, { nested = {} })

        -- Mutate merged outputs to ensure inputs are untouched
        deep.tls.min = 'mutated'

        return {
            shallow = shallow,
            deep = deep,
            orig_min = a.tls.min,
            replace_with_null = replace_with_null,
            replace_with_empty_array = replace_with_empty_array,
            merge_with_empty_object = merge_with_empty_object,
            empty_merge = merge(),
            empty_deep_merge = deep_merge(),
        }
        ",
    );

    assert_eq!(
        val,
        json!({
            "shallow": {
                "host": "b",
                "tls": { "max": "1.3", "alpn": ["http/1.1"] },
                "list": [3],
            },
            "deep": {
                "host": "b",
                "extra": true,
                "tls": { "min": "mutated", "max": "1.3", "sni": "example.com", "alpn": ["http/1.1"] },
                "list": [3],
            },
            "orig_min": "1.2",
            "replace_with_null": { "nested": null },
            "replace_with_empty_array": { "nested": [] },
            "merge_with_empty_object": { "nested": { "k": 1 } },
            "empty_merge": {},
            "empty_deep_merge": {},
        })
    );

    assert!(eval_lua_err("return merge({}, 1)").contains("table"));
    assert!(eval_lua_err("return deep_merge({}, 'bad')").contains("table"));
    let cycle_err = eval_lua_err(
        r"
        local t = {}
        t.loop = t
        return deep_merge({}, t)
        ",
    );
    assert!(
        cycle_err.contains("cyclic"),
        "unexpected error: {cycle_err}"
    );
}

#[test]
fn keys_and_values_are_deterministic() {
    let val = eval_lua(
        r"
        local obj = { zebra = 3, alpha = 1, middle = 2 }
        local arr = { 'first', 'second', 'third' }
        return {
            obj_keys = keys(obj),
            obj_values = values(obj),
            arr_keys = keys(arr),
            arr_values = values(arr),
        }
        ",
    );

    assert_eq!(
        val,
        json!({
            "obj_keys": ["alpha", "middle", "zebra"],
            "obj_values": [1, 2, 3],
            "arr_keys": [1, 2, 3],
            "arr_values": ["first", "second", "third"],
        })
    );

    assert!(eval_lua_err("return keys(123)").contains("table"));
    assert!(eval_lua_err("return values('abc')").contains("table"));
}

#[test]
fn functional_list_helpers() {
    let val = eval_lua(
        r"
        local nums = range(1, 6)
        local doubled = map(nums, function(x) return x * 2 end)
        local evens = filter(nums, function(x) return x % 2 == 0 end)
        local expanded = flat_map({ 1, 2, 3 }, function(x) return { x, x * 10 } end)
        local combined = concat({ 'a', 'b' }, array({}), { 'c' })
        local deduped = unique({ 'b', 'a', 'b', 1, 1.0, 2, null, null, { k = 1 }, { k = 1 }, { k = 2 } })
        local grouped = group_by(
            {
                { name = 'api', env = 'prod' },
                { name = 'web', env = 'staging' },
                { name = 'auth', env = 'prod' },
            },
            function(svc) return svc.env end
        )
        return {
            nums = nums,
            doubled = doubled,
            evens = evens,
            expanded = expanded,
            combined = combined,
            deduped = deduped,
            has_two = contains({ 1, 2, 3 }, 2.0),
            has_table = contains({ { a = 1 }, { a = 2 } }, { a = 2 }),
            has_null = contains({ 1, null, 3 }, null),
            has_missing = contains({ 1, 2, 3 }, 99),
            slice_mid = slice({ 10, 20, 30, 40, 50 }, 2, 4),
            slice_tail = slice({ 10, 20, 30, 40, 50 }, 4),
            slice_neg = slice({ 10, 20, 30, 40, 50 }, -2),
            slice_neg_range = slice({ 10, 20, 30, 40, 50 }, -4, -2),
            range_step = range(1, 7, 2),
            range_down = range(6, 0, -2),
            grouped = grouped,
        }
        ",
    );

    assert_eq!(
        val,
        json!({
            "nums": [1, 2, 3, 4, 5, 6],
            "doubled": [2, 4, 6, 8, 10, 12],
            "evens": [2, 4, 6],
            "expanded": [1, 10, 2, 20, 3, 30],
            "combined": ["a", "b", "c"],
            "deduped": ["b", "a", 1, 2, null, { "k": 1 }, { "k": 2 }],
            "has_two": true,
            "has_table": true,
            "has_null": true,
            "has_missing": false,
            "slice_mid": [20, 30, 40],
            "slice_tail": [40, 50],
            "slice_neg": [40, 50],
            "slice_neg_range": [20, 30, 40],
            "range_step": [1, 3, 5, 7],
            "range_down": [6, 4, 2, 0],
            "grouped": {
                "prod": [
                    { "name": "api", "env": "prod" },
                    { "name": "auth", "env": "prod" },
                ],
                "staging": [
                    { "name": "web", "env": "staging" },
                ],
            },
        })
    );

    assert!(eval_lua_err("return range(1, 5, 0)").contains("zero"));
    assert!(eval_lua_err("return map({ a = 1 }, function(x) return x end)").contains("list"));
    assert!(eval_lua_err("return flat_map({ 1 }, function(x) return x end)").contains("list"));
    assert!(eval_lua_err("return group_by({ 1, 2 }, function(x) return x end)").contains("string"));
    assert!(
        eval_lua_err("return filter({ [1] = 'a', [3] = 'c' }, function() return true end)")
            .contains("sparse")
    );
}

#[test]
fn string_helpers_literal_and_bounded() {
    let val = eval_lua(
        r"
        return {
            trimmed = str.trim('  \t hello world \n '),
            split_csv = str.split('a,b,,c', ','),
            split_chars = str.split('ab', ''),
            starts_yes = str.starts_with('https://example.com', 'https://'),
            starts_no = str.starts_with('http://example.com', 'https://'),
            ends_yes = str.ends_with('example.com.', '.'),
            ends_no = str.ends_with('example.com', '.'),
            -- Verify literal replacement (Lua pattern chars must NOT be interpreted)
            replaced = str.replace('a.b%s(1)+a.b%s(1)', 'a.b%s(1)', 'X'),
            joined = str.join({ 'a', 'b', 'c' }, '::'),
            joined_empty = str.join(array({}), ','),
            joined_default_sep = str.join({ 'x', 'y' }),
        }
        ",
    );

    assert_eq!(
        val,
        json!({
            "trimmed": "hello world",
            "split_csv": ["a", "b", "", "c"],
            "split_chars": ["a", "b"],
            "starts_yes": true,
            "starts_no": false,
            "ends_yes": true,
            "ends_no": false,
            "replaced": "X+X",
            "joined": "a::b::c",
            "joined_empty": "",
            "joined_default_sep": "xy",
        })
    );

    assert!(eval_lua_err("return str.join({ 1, 2 }, ',')").contains("strings"));

    let oversize_replace = format!(
        "return str.replace(string.rep('a', 1000), 'a', string.rep('b', {}))",
        MAX_STRING_BYTES / 500
    );
    assert!(eval_lua_err(&oversize_replace).contains("maximum allowed size"));
}

#[test]
fn json_yaml_toml_codecs_are_deterministic_and_round_trip() {
    let val = eval_lua(
        r"
        local sample = { z = { 1, 2 }, a = { b = true, empty = array({}), n = null } }
        local json_str = json.encode(sample)
        local json_back = json.decode(json_str)

        local yaml_str = yaml.encode(sample)
        local yaml_back = yaml.decode(yaml_str)

        local toml_sample = { z = { 1, 2 }, a = { b = true, name = 'oxiroute' } }
        local toml_str = toml.encode(toml_sample)
        local toml_back = toml.decode(toml_str)

        return {
            json_str = json_str,
            json_back = json_back,
            yaml_str = yaml_str,
            yaml_back = yaml_back,
            toml_str = toml_str,
            toml_back = toml_back,
        }
        ",
    );

    assert_eq!(
        val["json_str"],
        json!(r#"{"a":{"b":true,"empty":[],"n":null},"z":[1,2]}"#)
    );
    assert_eq!(
        val["json_back"],
        json!({ "a": { "b": true, "empty": [], "n": null }, "z": [1, 2] })
    );
    assert_eq!(
        val["yaml_str"],
        json!("a:\n  b: true\n  empty: []\n  n: null\nz:\n  - 1\n  - 2\n")
    );
    assert_eq!(val["yaml_back"], val["json_back"]);
    assert_eq!(
        val["toml_back"],
        json!({ "a": { "b": true, "name": "oxiroute" }, "z": [1, 2] })
    );

    assert!(eval_lua_err("return json.decode('{invalid')").contains("json.decode"));
    assert!(eval_lua_err("return yaml.decode('&anchor foo')").contains("yaml.decode"));
    assert!(eval_lua_err("return toml.decode('= bad')").contains("toml.decode"));
    assert!(eval_lua_err("return toml.encode({ 1, 2 })").contains("toml.encode"));
    assert!(eval_lua_err("return toml.encode({ key = null })").contains("toml.encode"));

    let oversize_json = format!(
        "return json.decode(string.rep(' ', {}))",
        MAX_STRING_BYTES + 1
    );
    assert!(!eval_lua_err(&oversize_json).is_empty());
}

#[test]
fn base64_and_hash_helpers() {
    let val = eval_lua(
        r"
        local raw = 'hello?>world!'
        local std_enc = base64.encode(raw)
        local url_enc = base64.encode_url(raw)
        return {
            std_enc = std_enc,
            std_dec = base64.decode(std_enc),
            url_enc = url_enc,
            url_dec = base64.decode_url(url_enc),
            url_dec_padded = base64.decode_url('Zg=='),
            empty_enc = base64.encode(''),
            empty_dec = base64.decode(''),
            sha_empty = hash.sha256(''),
            sha_hello = hash.sha256('hello'),
        }
        ",
    );

    assert_eq!(
        val,
        json!({
            "std_enc": "aGVsbG8/PndvcmxkIQ==",
            "std_dec": "hello?>world!",
            "url_enc": "aGVsbG8_PndvcmxkIQ",
            "url_dec": "hello?>world!",
            "url_dec_padded": "f",
            "empty_enc": "",
            "empty_dec": "",
            "sha_empty": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "sha_hello": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        })
    );

    assert!(eval_lua_err("return base64.decode('!!!not-base64!!!')").contains("base64.decode"));
    assert!(
        eval_lua_err("return base64.decode_url('aGVsbG8/PndvcmxkIQ==')")
            .contains("base64.decode_url")
    );

    let oversize_b64 = format!("return base64.encode(string.rep('a', {MAX_STRING_BYTES}))");
    assert!(!eval_lua_err(&oversize_b64).is_empty());

    let oversize_hash = format!(
        "return hash.sha256(string.rep('a', {}))",
        MAX_STRING_BYTES + 1
    );
    assert!(eval_lua_err(&oversize_hash).contains("maximum allowed size"));
}

#[test]
fn network_helpers_and_validation() {
    let val = eval_lua(
        r"
        return {
            join_v4 = net.join_host_port('127.0.0.1', 8080),
            join_host = net.join_host_port('api.example.com', 443),
            join_v6 = net.join_host_port('::1', 8080),
            join_v6_bracketed = net.join_host_port('[2001:db8::1]', 8443),
            v4_yes = net.is_ipv4('10.0.0.1'),
            v4_no = net.is_ipv4('::1'),
            v6_yes = net.is_ipv6('2001:db8::1'),
            v6_no = net.is_ipv6('10.0.0.1'),
            ip_v4 = net.is_ip('192.168.1.1'),
            ip_v6 = net.is_ip('::1'),
            ip_no = net.is_ip('example.com'),
            cidr_v4 = net.is_cidr('10.0.0.0/8'),
            cidr_v6 = net.is_cidr('2001:db8::/32'),
            cidr_bad_prefix = net.is_cidr('10.0.0.0/33'),
            cidr_plain_ip = net.is_cidr('10.0.0.1'),
            in_v4 = net.cidr_contains('10.0.0.0/8', '10.1.2.3'),
            out_v4 = net.cidr_contains('10.0.0.0/8', '192.168.0.1'),
            in_v6 = net.cidr_contains('2001:db8::/32', '2001:db8::dead:beef'),
            out_v6 = net.cidr_contains('2001:db8::/32', '2001:db9::1'),
            cross_family = net.cidr_contains('10.0.0.0/8', '::1'),
            norm_upper = net.normalize_host('API.Example.COM.'),
            norm_plain = net.normalize_host('api.example.com'),
            norm_double_dot = net.normalize_host('Example.COM..'),
        }
        ",
    );

    assert_eq!(
        val,
        json!({
            "join_v4": "127.0.0.1:8080",
            "join_host": "api.example.com:443",
            "join_v6": "[::1]:8080",
            "join_v6_bracketed": "[2001:db8::1]:8443",
            "v4_yes": true,
            "v4_no": false,
            "v6_yes": true,
            "v6_no": false,
            "ip_v4": true,
            "ip_v6": true,
            "ip_no": false,
            "cidr_v4": true,
            "cidr_v6": true,
            "cidr_bad_prefix": false,
            "cidr_plain_ip": false,
            "in_v4": true,
            "out_v4": false,
            "in_v6": true,
            "out_v6": false,
            "cross_family": false,
            "norm_upper": "api.example.com",
            "norm_plain": "api.example.com",
            "norm_double_dot": "example.com.",
        })
    );

    assert!(eval_lua_err("return net.join_host_port('127.0.0.1', 0)").contains("port"));
    assert!(eval_lua_err("return net.join_host_port('127.0.0.1', 65536)").contains("port"));
    assert!(eval_lua_err("return net.join_host_port('', 80)").contains("empty"));
    assert!(eval_lua_err("return net.join_host_port('bad:host', 80)").contains("invalid host"));
    assert!(eval_lua_err("return net.cidr_contains('not-a-cidr', '10.0.0.1')").contains("CIDR"));
    assert!(eval_lua_err("return net.cidr_contains('10.0.0.0/8', 'not-an-ip')").contains("IP"));
}

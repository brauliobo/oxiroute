use super::Question;
use crate::rtmp::api::management::{Route, match_route};

#[test]
fn the_route_question_is_a_read_only_endpoint() {
    assert!(matches!(
        match_route("/api/v1/routes/explain?path=%2Fx"),
        Some(Route::Explain(Some("path=%2Fx")))
    ));
}

#[test]
fn a_question_names_a_path_and_defaults_to_get() {
    let question =
        Question::parse(Some("host=example.com%3A8080&path=%2Fa%20b&listener=edge")).unwrap();
    assert_eq!(question.host.as_deref(), Some("example.com:8080"));
    assert_eq!(question.path, "/a b");
    assert_eq!(question.listener.as_deref(), Some("edge"));
    assert_eq!(question.method, http::Method::GET);
    let post = Question::parse(Some("path=%2F&method=POST")).unwrap();
    assert_eq!(post.method, http::Method::POST);
}

#[test]
fn a_question_without_a_usable_path_or_with_unknown_fields_is_refused() {
    for query in [
        None,
        Some("host=example.com"),
        Some("path=x"),
        Some("path=%2Fa%2F..%2Fb"),
        Some("path=%2F&method=NOT%20A%20METHOD"),
        Some("path=%2F&color=red"),
        Some("path=%zz"),
    ] {
        assert!(Question::parse(query).is_err(), "{query:?}");
    }
}

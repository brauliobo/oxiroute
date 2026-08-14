mod cache_validation;
mod composition;
mod defaults;
mod forward_validation;
mod http_validation;
mod lexical;
mod model;
mod validated;
mod validation;

pub use composition::{ConfigCompositionError, compose_validated_configs};
pub use lexical::{
    LexicalError, canonical_certificate_dns_name, canonical_dns_name, canonical_ip,
    canonicalize_http_path, is_unambiguous_http_path,
    normalize_unix_socket_path as normalize_unix_path,
    validate_absolute_file_path as validate_file_path,
};
pub use model::*;
pub use validated::ValidatedConfig;
pub use validation::{
    validate_health_check_config, validate_upstream_pool_definitions, validate_upstream_pools,
};

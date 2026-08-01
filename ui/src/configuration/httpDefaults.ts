import {
  HTTP_RETRY_TRIGGERS,
  type HttpAccessPolicyConfig,
  type HttpProxyPolicyConfig,
  type HttpRedirectLocationConfig,
  type HttpRouteActionConfig,
  type HttpRouteConfig,
  type HttpUpstreamHostConfig,
} from '../config'
export { HTTP_RETRY_TRIGGERS }

export function defaultHttpRoute(): HttpRouteConfig {
  return {
    host: null,
    path: { kind: 'segment_prefix', value: '/' },
    methods: [],
    access_policy: null,
    policy: {
      max_request_body_bytes: 10_485_760,
      connect_timeout_ms: 30_000,
      read_timeout_ms: 30_000,
      write_timeout_ms: 30_000,
      request_buffering: false,
      response_buffering: false,
    },
    action: defaultHttpAction('proxy'),
  }
}

export function defaultHttpAccessPolicy(): HttpAccessPolicyConfig {
  return {
    type: 'bearer_token_file',
    token_file_path: '',
    header_name: 'authorization',
    realm: null,
  }
}

export function defaultHttpAction(type: HttpRouteActionConfig['type']): HttpRouteActionConfig {
  switch (type) {
    case 'proxy':
      return { type, upstream_pool: '', policy: defaultHttpProxyPolicy() }
    case 'fixed_response':
      return { type, status: 200, body: '', headers: [] }
    case 'redirect':
      return { type, status: 302, location: defaultRedirectLocation('literal'), headers: [] }
    case 'static_files':
      return {
        type,
        root_directory: '',
        path_mapping: 'root',
        index_files: ['index.html'],
        internal_index_redirects: false,
        directory_redirects: false,
        spa_fallback: null,
        try_files: [],
        autoindex: false,
        autoindex_exact_size: true,
        autoindex_local_time: false,
        etag: true,
        mime: { default_type: null, types: [] },
        headers: [],
        error_responses: [],
      }
  }
}

export function defaultHttpProxyPolicy(): HttpProxyPolicyConfig {
  return {
    upstream_host: defaultUpstreamHost('preserve_incoming'),
    request_headers: [],
    response_headers: [],
    response_cookie_path_rewrites: [],
    response_cookie_attributes: [],
    retry: {
      max_retries: 0,
      target: 'next_server',
      delay_ms: 0,
      final_redispatch: false,
      triggers: [...HTTP_RETRY_TRIGGERS],
      method_safety: 'get_head',
      body_safety: 'empty',
    },
    cache: null,
  }
}

export function defaultUpstreamHost(type: HttpUpstreamHostConfig['type']): HttpUpstreamHostConfig {
  switch (type) {
    case 'preserve_incoming': return { type }
    case 'nginx_host': return { type, fallback: 'localhost' }
    case 'endpoint': return { type, unix_fallback: null }
    case 'literal': return { type, value: '' }
  }
}

export function defaultRedirectLocation(
  kind: HttpRedirectLocationConfig['kind'],
): HttpRedirectLocationConfig {
  return kind === 'request_template'
    ? { kind, value: '$scheme://$host$request_uri', nginx_host_fallback: null }
    : { kind, value: '/' }
}

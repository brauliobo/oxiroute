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
      return { type, status: 302, location: defaultRedirectLocation('literal') }
    case 'static_files':
      return { type, root_directory: '', index_files: ['index.html'], spa_fallback: null }
  }
}

export function defaultHttpProxyPolicy(): HttpProxyPolicyConfig {
  return {
    upstream_host: defaultUpstreamHost('preserve_incoming'),
    request_headers: [],
    response_headers: [],
    response_cookie_path_rewrites: [],
    retry: {
      max_retries: 0,
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
    case 'endpoint': return { type, unix_fallback: null }
    case 'literal': return { type, value: '' }
  }
}

export function defaultRedirectLocation(
  kind: HttpRedirectLocationConfig['kind'],
): HttpRedirectLocationConfig {
  return { kind, value: kind === 'request_template' ? '$scheme://$host$request_uri' : '/' }
}

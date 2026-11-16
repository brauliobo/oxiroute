import type { AccessLogConfig } from './model'

export type RtmpRecorderStart = 'continuous' | 'manual'
export type RtmpAclAction = 'allow' | 'deny'
export type RtmpTokenSource = 'stream_query'
export type RtmpNotifyMethod = 'get' | 'post'
export type RtmpRtmpsPolicy = 'disabled' | 'allowed' | 'required'
export type RtmpTransport = 'rtmp' | 'rtmps'
export type RtmpExecMode = 'command' | 'transcode'
export type RtmpExecTrigger = 'publisher' | 'publish_done'
export type RtmpExecFilesystemPolicy = 'working_directory' | 'host'
export type RtmpExecNetworkPolicy = 'disabled' | 'inherited'
export type RtmpVodSource =
  | { type: 'local'; name: string; root_directory: string }
  | { type: 'http'; name: string; origin: string }

export type RtmpHlsFragmentNaming = 'sequential' | 'timestamp' | 'system'

export interface RtmpHlsVariantConfig {
  name: string
  bandwidth: number
  codecs: string | null
  width: number | null
  height: number | null
}

export interface RtmpHlsKeyConfig {
  rotation_segments: number
  url_prefix: string
}

export interface RtmpHlsPolicyConfig {
  root_directory: string
  segment_duration_ms: number
  max_segment_duration_ms: number
  playlist_length_ms: number
  fragment_naming: RtmpHlsFragmentNaming
  nested: boolean
  cleanup: boolean
  variants: RtmpHlsVariantConfig[]
  keys: RtmpHlsKeyConfig | null
  max_segment_bytes: number
  max_queue_messages: number
  max_storage_bytes: number
  max_storage_files: number
  max_active_streams: number
}

export interface RtmpDashPolicyConfig {
  root_directory: string
  segment_duration_ms: number
  max_segment_duration_ms: number
  playlist_length_ms: number
  segment_naming: 'sequential' | 'timestamp' | 'system'
  nested: boolean
  cleanup: boolean
  max_segment_bytes: number
  max_queue_messages: number
  max_storage_bytes: number
  max_storage_files: number
  max_active_streams: number
}

export interface RtmpApplicationConfig {
  name: string
  live: boolean
  idle_streams: boolean
  publish: RtmpAccessPolicyConfig
  play: RtmpAccessPolicyConfig
  limits: RtmpSessionCeilingsConfig
  push_targets: RtmpPushTargetConfig[]
  pull_targets: RtmpPullTargetConfig[]
  relay: RtmpRelayPolicyConfig
  callbacks: RtmpCallbackConfig
  fanout: {
    max_subscribers: number
    max_queue_messages_per_subscriber: number
    max_queue_bytes_per_subscriber: number
  }
  vod: RtmpVodPolicyConfig | null
  hls?: RtmpHlsPolicyConfig | null
  dash?: RtmpDashPolicyConfig | null
  recorders: RtmpRecorderConfig[]
}

export interface RtmpAccessPolicyConfig {
  rules: RtmpAccessRuleConfig[]
  token: RtmpTokenPolicyConfig | null
}

export interface RtmpAccessRuleConfig {
  action: RtmpAclAction
  network: string
}

export interface RtmpTokenPolicyConfig {
  source: RtmpTokenSource
  parameter: string
  secret: string
}

export interface RtmpSessionCeilingsConfig {
  max_connections: number
  max_publishers: number
  max_viewers: number
}

export interface RtmpRecorderConfig {
  name: string
  start: RtmpRecorderStart
  root_directory: string
  record_mask: { audio: boolean; video: boolean; keyframes: boolean }
  suffix_template: string
  append_unix_seconds: boolean
  append: boolean
  lock: boolean
  max_size: number | null
  max_frames: number | null
  notify: boolean
  timezone: string
  time_basis: 'segment_start' | 'segment_end'
  segment_naming: 'safe_unique' | 'nginx_compatible'
  rotation_interval_ms: number | null
  max_queue_messages: number
  max_queue_bytes: number
  shutdown_timeout_ms: number
  max_storage_bytes: number | null
  max_storage_files: number | null
  max_active_recorders: number
}

export interface RtmpCallbackConfig {
  on_connect: string | null
  on_disconnect: string | null
  on_publish: string | null
  on_publish_done: string | null
  on_play: string | null
  on_play_done: string | null
  on_done: string | null
  on_update: string | null
  notify_method: RtmpNotifyMethod
  timeout_ms: number
  notify_update_timeout_ms: number
  notify_update_strict: boolean
  notify_relay_redirect: boolean
}

export interface RtmpOutboundPolicyConfig {
  allow_domains: string[]
  deny_domains: string[]
  allow_cidrs: string[]
  deny_cidrs: string[]
  deny_private: boolean
  rtmps: RtmpRtmpsPolicy
  max_chain_depth: number
}

export interface RtmpRelayPolicyConfig {
  max_queue_messages: number
  max_queue_bytes: number
  buffer_ms: number
  push_reconnect_ms: number
  pull_reconnect_ms: number
  dns_refresh_ms: number
  connect_timeout_ms: number
  handshake_timeout_ms: number
}

export interface RtmpCredentialReferenceConfig {
  username: string
  secret_file: string
}

export interface RtmpPushTargetConfig {
  host: string
  port: number
  application: string
  scheme: RtmpTransport
  stream_name: string | null
  tc_url: string | null
  flash_version: string | null
  credentials: RtmpCredentialReferenceConfig | null
}

export interface RtmpPullTargetConfig {
  host: string
  port: number
  application: string
  stream_name: string
  scheme: RtmpTransport
  tc_url: string | null
  flash_version: string | null
  credentials: RtmpCredentialReferenceConfig | null
}

export interface RtmpVodPolicyConfig {
  sources: RtmpVodSource[]
  max_sessions: number
  max_file_bytes: number
  max_duration_ms: number
}

export interface RtmpExecEnvironmentConfig {
  name: string
  value: string
}

export interface RtmpExecProfileConfig {
  name: string
  application: string
  mode: RtmpExecMode
  trigger: RtmpExecTrigger
  executable: string
  arguments: string[]
  environment: RtmpExecEnvironmentConfig[]
  working_directory: string
  filesystem: RtmpExecFilesystemPolicy
  network: RtmpExecNetworkPolicy
  timeout_ms: number
  shutdown_timeout_ms: number
  max_processes: number
  max_queue_messages: number
  max_queue_bytes: number
  max_stdout_bytes: number
  max_stderr_bytes: number
  respawn: boolean
  respawn_delay_ms: number
  max_respawns: number
}

export interface RtmpAutoPushConfig {
  enabled: boolean
  socket_dir: string
  secret_file: string | null
  reconnect_ms: number
  connect_timeout_ms: number
  handshake_timeout_ms: number
  max_peers: number
  max_queue_messages: number
  max_queue_bytes: number
  max_streams: number
}

export interface RtmpServiceConfig {
  name: string
  outbound_chunk_size: number
  access_log: AccessLogConfig | null
  outbound_policy: RtmpOutboundPolicyConfig
  callbacks: RtmpCallbackConfig
  auto_push: RtmpAutoPushConfig
  exec_profiles?: RtmpExecProfileConfig[]
  applications: RtmpApplicationConfig[]
}
